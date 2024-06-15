mod config;
mod post;
mod util;

use crate::config::Config;
use crate::config::PostConfig;
use crate::config::PostConfigPrivacy;
use crate::post::Post;
use crate::post::PostDiff;
use crate::post::PostFile;
use crate::post::PostPrivacy;
use anyhow::ensure;
use anyhow::Context;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use sha2::Digest;
use sha2::Sha256;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Cache {
    /// The old post
    pub post: Post,
}

#[derive(Debug, argh::FromArgs)]
#[argh(description = "a CLI to sync folders to imgchest.com")]
pub struct Options {
    #[argh(
        option,
        long = "token",
        short = 't',
        description = "the API token to use"
    )]
    pub token: String,

    #[argh(
        option,
        long = "input",
        short = 'i',
        description = "the directory to sync posts from"
    )]
    pub input: Utf8PathBuf,

    #[argh(
        switch,
        long = "no-read-cache",
        description = "avoid reading the cache"
    )]
    pub no_read_cache: bool,

    #[argh(
        switch,
        long = "print-diffs",
        description = "whether the generated post diffs should be printed"
    )]
    pub print_diffs: bool,
}

fn main() -> anyhow::Result<()> {
    let options: Options = argh::from_env();

    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    tokio_rt.block_on(async_main(options))
}

async fn async_main(options: Options) -> anyhow::Result<()> {
    let client = imgchest::Client::new();
    client.set_token(options.token);

    let mut dir_iter = tokio::fs::read_dir(&options.input).await?;
    while let Some(entry) = dir_iter.next_entry().await? {
        let file_type = entry.file_type().await?;
        let entry_path = entry.path();
        let entry_path: &Utf8Path = entry_path.as_path().try_into()?;

        if !file_type.is_dir() {
            continue;
        }

        let dir_path = options.input.join(entry_path);
        let config_path = dir_path.join("imgchest-sync.toml");
        let cache_path = dir_path.join(".imgchest-sync-cache.toml");

        let mut config = match crate::util::try_read_to_string(&config_path)
            .await
            .context("failed to read config file")?
        {
            Some(config_raw) => Config::new(&config_raw).context("failed to parse config file")?,
            None => continue,
        };

        let mut cache = None;
        if !options.no_read_cache {
            cache = match crate::util::try_read_to_string(&cache_path)
                .await
                .context("failed to read cache file")?
            {
                Some(cache_raw) => {
                    match toml::from_str::<Cache>(&cache_raw).context("failed to parse cache file")
                    {
                        Ok(cache) => Some(cache),
                        Err(error) => {
                            eprintln!("{error:?}");
                            None
                        }
                    }
                }
                None => None,
            };
        }

        let mut post_config = config.post_mut();

        let mut new_post = create_post_from_post_config(&dir_path, &post_config).await?;

        match post_config.id() {
            Some(id) => {
                let old_post = match cache.as_ref() {
                    Some(cache) => &cache.post,
                    None => {
                        let post = create_post_from_online(&client, id)
                            .await
                            .context("failed to create post from online")?;

                        cache = Some(Cache { post });
                        &cache.as_ref().expect("missing cache").post
                    }
                };

                let diffs = generate_post_diffs(old_post, &new_post)
                    .context("failed to generate post diffs")?;
                let diff_empty = diffs
                    .iter()
                    .all(|diff| matches!(diff, PostDiff::RetainFile { .. }));

                if options.print_diffs {
                    println!("Diffs: {diffs:#?}");
                }

                if !diff_empty {
                    update_online_post(&client, id, diffs, old_post, &mut new_post, &cache_path)
                        .await?;
                }
            }
            None => {
                let mut builder = imgchest::CreatePostBuilder::new();
                builder.title(new_post.title.clone());

                for file in new_post.files.iter() {
                    let path = file.path.as_ref().context("missing path")?;
                    let file = imgchest::UploadPostFile::from_path(&path)
                        .await
                        .with_context(|| format!("failed to open image at \"{path}\""))?;

                    builder.image(file);
                }

                let imgchest_post = client
                    .create_post(builder)
                    .await
                    .context("failed to create new post")?;

                post_config.set_id(Some(&*imgchest_post.id));

                ensure!(imgchest_post.images.len() == new_post.files.len());
                for (file, imgchest_image) in new_post
                    .files
                    .iter_mut()
                    .zip(Vec::from(imgchest_post.images).into_iter())
                {
                    file.id = Some(imgchest_image.id.into());
                }

                let tmp_config_path = nd_util::with_push_extension(&config_path, "temp");
                tokio::fs::write(&tmp_config_path, config.to_string())
                    .await
                    .context("failed to write new config")?;
                tokio::fs::rename(tmp_config_path, config_path).await?;
            }
        }

        let cache = match cache {
            Some(mut cache) => {
                cache.post = new_post;
                cache
            }
            None => Cache { post: new_post },
        };

        let mut cache_str = String::new();
        cache_str.push_str("# This file was autogenerated by imgchest-sync.\n");
        cache_str.push_str("# DO NOT EDIT.\n");
        cache_str.push('\n');
        cache_str += &toml::to_string(&cache)?;

        let tmp_cache_path = nd_util::with_push_extension(&cache_path, "temp");
        tokio::fs::write(&tmp_cache_path, cache_str)
            .await
            .context("failed to write new cache")?;
        tokio::fs::rename(tmp_cache_path, cache_path).await?;
    }

    Ok(())
}

async fn create_post_from_post_config(
    dir_path: &Utf8Path,
    post_config: &PostConfig<'_>,
) -> anyhow::Result<Post> {
    let dir_name = dir_path.file_name().context("missing dir name")?;

    let title = post_config.title().unwrap_or(dir_name).into();
    let privacy = match post_config.privacy().unwrap_or(PostConfigPrivacy::Hidden) {
        PostConfigPrivacy::Public => PostPrivacy::Public,
        PostConfigPrivacy::Hidden => PostPrivacy::Hidden,
        PostConfigPrivacy::Secret => PostPrivacy::Secret,
    };
    let nsfw = post_config.nsfw().unwrap_or(false);
    let files = {
        let files_config = post_config.files();
        let mut files = Vec::with_capacity(files_config.len());
        for file in files_config.iter() {
            let path = Utf8Path::new(file.path());
            let path = if path.is_relative() {
                dir_path.join(path)
            } else {
                path.into()
            };

            let sha256 = {
                let path = path.clone();
                tokio::task::spawn_blocking(move || {
                    let mut file = std::fs::File::open(path)?;

                    let mut hasher = Sha256::new();
                    std::io::copy(&mut file, &mut hasher)?;
                    let hash = hasher.finalize();
                    let hex_hash = base16ct::lower::encode_string(&hash);

                    anyhow::Ok(hex_hash)
                })
                .await??
            };

            files.push(PostFile {
                description: String::new(),
                sha256,
                path: Some(path),
                id: None,
            });
        }
        files
    };

    Ok(Post {
        title,
        privacy,
        nsfw,
        files,
    })
}

async fn create_post_from_online(client: &imgchest::Client, id: &str) -> anyhow::Result<Post> {
    let imgchest_post = client.get_post(id).await?;

    let title = imgchest_post
        .title
        .map(String::from)
        .unwrap_or_else(String::new);
    let privacy = match imgchest_post.privacy {
        imgchest::PostPrivacy::Public => PostPrivacy::Public,
        imgchest::PostPrivacy::Hidden => PostPrivacy::Hidden,
        imgchest::PostPrivacy::Secret => PostPrivacy::Secret,
    };
    let nsfw = imgchest_post.nsfw;
    let files = {
        let mut files = Vec::new();
        for image in Vec::from(imgchest_post.images).into_iter() {
            let handle = tokio::runtime::Handle::current();
            let mut image_response = client
                .client
                .get(&*image.link)
                .send()
                .await?
                .error_for_status()?;
            let sha256 = tokio::task::spawn_blocking(move || {
                let mut hasher = Sha256::new();
                while let Some(chunk) = handle.block_on(image_response.chunk())? {
                    hasher.update(chunk);
                }

                let hash = hasher.finalize();
                let hex_hash = base16ct::lower::encode_string(&hash);

                anyhow::Ok(hex_hash)
            })
            .await??;

            files.push(PostFile {
                description: String::new(),
                sha256,
                path: None,
                id: Some(image.id.into()),
            });
        }
        files
    };

    Ok(Post {
        title,
        privacy,
        nsfw,
        files,
    })
}

async fn update_online_post(
    client: &imgchest::Client,
    id: &str,
    diffs: Vec<PostDiff>,
    old_post: &Post,
    new_post: &mut Post,
    cache_path: &Utf8Path,
) -> anyhow::Result<()> {
    let mut update_post_builder = None;
    let mut files_to_remove = Vec::new();
    let mut files_to_add_indicies = Vec::new();
    let mut files_to_add = Vec::new();
    for diff in diffs {
        match diff {
            PostDiff::EditTitle { title } => {
                update_post_builder
                    .get_or_insert_with(imgchest::UpdatePostBuilder::new)
                    .title(title);
            }
            PostDiff::EditPrivacy { privacy } => {
                update_post_builder
                    .get_or_insert_with(imgchest::UpdatePostBuilder::new)
                    .privacy(match privacy {
                        PostPrivacy::Public => imgchest::PostPrivacy::Public,
                        PostPrivacy::Hidden => imgchest::PostPrivacy::Hidden,
                        PostPrivacy::Secret => imgchest::PostPrivacy::Secret,
                    });
            }
            PostDiff::EditNsfw { nsfw } => {
                update_post_builder
                    .get_or_insert_with(imgchest::UpdatePostBuilder::new)
                    .nsfw(nsfw);
            }
            PostDiff::RetainFile { index } => {
                new_post.files[index].id = Some(
                    old_post.files[index]
                        .id
                        .as_ref()
                        .context("old post missing id")?
                        .clone(),
                );
            }
            PostDiff::AddFile { index } => {
                let path = new_post.files[index]
                    .path
                    .as_ref()
                    .context("missing path")?;
                let file = imgchest::UploadPostFile::from_path(path).await?;
                files_to_add.push(file);
                files_to_add_indicies.push(index);
            }
            PostDiff::RemoveFile { index } => {
                let id = old_post.files[index]
                    .id
                    .as_ref()
                    .context("missing id of file to remove")?;
                files_to_remove.push(id);
            }
        }
    }

    // Nuke the cache.
    // We cannot perform the diff atomically.
    // If the update is interrupted, the cache will reflect bad data.
    tokio::fs::remove_file(&cache_path).await?;

    if let Some(update_post_builder) = update_post_builder {
        client.update_post(id, update_post_builder).await?;
    }

    for id in files_to_remove.iter() {
        client.delete_file(id).await?;
    }

    if !files_to_add.is_empty() {
        let imgchest_post = client.add_post_images(id, files_to_add).await?;
        for index in files_to_add_indicies {
            new_post.files[index].id = Some(imgchest_post.images[index].id.clone().into());
        }
    }

    Ok(())
}

fn generate_post_diffs(old: &Post, new: &Post) -> anyhow::Result<Vec<PostDiff>> {
    ensure!(!old.files.is_empty(), "old post has no files");
    ensure!(!new.files.is_empty(), "new post has no files");

    let mut diffs = Vec::new();
    if old.title != new.title {
        diffs.push(PostDiff::EditTitle {
            title: new.title.clone(),
        });
    }
    if old.privacy != new.privacy {
        diffs.push(PostDiff::EditPrivacy {
            privacy: new.privacy,
        });
    }
    if old.nsfw != new.nsfw {
        diffs.push(PostDiff::EditNsfw { nsfw: new.nsfw });
    }

    // Ideally, we would diff and only upload what is changed.
    // However, the imgchest api is horriblly handicapped:
    // 1. We cannot reorder files without deleting and recreating them.
    // 2. We can only change a file description, not remove it.
    // 3. We cannot insert files at arbitrary indicies.
    //
    // While diffing would give us an advantage in some cases,
    // most of the time we would just throw out our calculations
    // or be forced to use some heurisitics to convert our diffs into
    // something the API can use.
    //
    // As a result, we will use a simpler, faster algorithm.
    // We will skip all initial files that are not changed and have the same description.
    // When we reach an index where there is a mismatch, delete everything past it.
    // Then, add the files from the new post.

    let mut prefix_index = 0;
    while let (Some(old_file), Some(new_file)) =
        (old.files.get(prefix_index), new.files.get(prefix_index))
    {
        // TODO: Account for description.
        if old_file.sha256 != new_file.sha256 {
            break;
        }

        diffs.push(PostDiff::RetainFile {
            index: prefix_index,
        });

        prefix_index += 1;
    }

    for index in prefix_index..old.files.len() {
        diffs.push(PostDiff::RemoveFile { index });
    }

    for index in prefix_index..new.files.len() {
        // Since we removed all the posts with the earlier diff,
        // The current old post object is a prefix of the new post object.
        // Therefore, the indicies of the new post work with the old one.
        diffs.push(PostDiff::AddFile { index });
    }

    // TODO: Emit description diffs

    anyhow::Ok(diffs)
}

#[cfg(test)]
mod test {
    use super::*;

    const SHA256_A: &str = "a";
    const SHA256_B: &str = "b";

    #[test]
    fn generate_post_diffs_works() {
        let old_post = Post {
            title: String::from("title"),
            privacy: PostPrivacy::Hidden,
            nsfw: false,
            files: vec![PostFile {
                description: String::new(),
                sha256: SHA256_B.into(),
                id: None,
                path: None,
            }],
        };
        let new_post = Post {
            title: String::from("title"),
            privacy: PostPrivacy::Hidden,
            nsfw: false,
            files: vec![PostFile {
                description: String::new(),
                sha256: SHA256_A.into(),
                id: None,
                path: None,
            }],
        };

        let actual_diffs =
            generate_post_diffs(&old_post, &new_post).expect("failed to generate diffs");
        let expected_diffs = vec![
            PostDiff::RemoveFile { index: 0 },
            PostDiff::AddFile { index: 0 },
        ];
        assert!(actual_diffs == expected_diffs);

        let old_post = Post {
            title: String::from("title"),
            privacy: PostPrivacy::Hidden,
            nsfw: false,
            files: vec![
                PostFile {
                    description: String::new(),
                    sha256: SHA256_A.into(),
                    id: None,
                    path: None,
                },
                PostFile {
                    description: String::new(),
                    sha256: SHA256_A.into(),
                    id: None,
                    path: None,
                },
            ],
        };
        let new_post = Post {
            title: String::from("title"),
            privacy: PostPrivacy::Hidden,
            nsfw: false,
            files: vec![PostFile {
                description: String::new(),
                sha256: SHA256_A.into(),
                id: None,
                path: None,
            }],
        };

        let actual_diffs =
            generate_post_diffs(&old_post, &new_post).expect("failed to generate diffs");
        let expected_diffs = vec![
            PostDiff::RetainFile { index: 0 },
            PostDiff::RemoveFile { index: 1 },
        ];
        assert!(actual_diffs == expected_diffs);

        let old_post = Post {
            title: String::from("title"),
            privacy: PostPrivacy::Hidden,
            nsfw: false,
            files: vec![PostFile {
                description: String::new(),
                sha256: SHA256_A.into(),
                id: None,
                path: None,
            }],
        };
        let new_post = Post {
            title: String::from("title"),
            privacy: PostPrivacy::Hidden,
            nsfw: false,
            files: vec![PostFile {
                description: String::new(),
                sha256: SHA256_A.into(),
                id: None,
                path: None,
            }],
        };
        let actual_diffs =
            generate_post_diffs(&old_post, &new_post).expect("failed to generate diffs");
        let expected_diffs = vec![PostDiff::RetainFile { index: 0 }];
        assert!(actual_diffs == expected_diffs);
    }
}
