mod commands;
mod config;
mod post;
mod util;

use crate::config::Config;
use crate::config::PostConfig;
use crate::config::PostConfigPrivacy;
use crate::config::UserConfig;
use crate::post::Post;
use crate::post::PostDiff;
use crate::post::PostFile;
use crate::post::PostPrivacy;
use anyhow::ensure;
use anyhow::Context;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use directories::ProjectDirs;
use regex::Regex;
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
    pub token: Option<String>,

    #[argh(
        option,
        long = "input",
        short = 'i',
        description = "the directory to sync posts from"
    )]
    pub input: Option<Utf8PathBuf>,

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

    #[argh(
        option,
        long = "filter-regex",
        description = "only process directory entry names accepted by the provided regex"
    )]
    pub filter_regex: Option<String>,

    #[argh(subcommand)]
    subcommand: Option<Subcommand>,
}

#[derive(Debug, argh::FromArgs)]
#[argh(subcommand)]
enum Subcommand {
    Config(self::commands::config::Options),
}

fn main() -> anyhow::Result<()> {
    let options: Options = argh::from_env();

    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    tokio_rt.block_on(async_main(options))
}

async fn async_main(options: Options) -> anyhow::Result<()> {
    let project_dirs =
        ProjectDirs::from("", "", "imgchest-sync").context("failed to get config directory")?;
    let config_dir = project_dirs.config_dir();
    tokio::fs::create_dir_all(&config_dir)
        .await
        .context("failed to create config directory")?;
    let config_path = config_dir.join("config.toml");
    let config = {
        let config_str = crate::util::try_read_to_string(&config_path)
            .await?
            .unwrap_or(String::new());
        UserConfig::new(&config_str).context("failed to parse user config")?
    };

    match options.subcommand {
        Some(Subcommand::Config(options)) => {
            self::commands::config::exec(options, &config_path, config).await?;
        }
        None => {
            let client = imgchest::Client::new();
            let token = options
                .token
                .as_deref()
                .or_else(|| config.token())
                .context(
                "missing API token. Specify it either with the --token flag or in the user config.",
            )?;
            client.set_token(token);

            exec(options, client).await?
        }
    }

    Ok(())
}

async fn exec(options: Options, client: imgchest::Client) -> anyhow::Result<()> {
    let input = options
        .input
        .as_ref()
        .context("missing input directory. Specify it with --input")?;
    let filter_regex = options
        .filter_regex
        .map(|filter_regex| {
            Regex::new(&format!("^{filter_regex}$")).context("invalid filter regex")
        })
        .transpose()?;

    let mut dir_iter = tokio::fs::read_dir(input).await?;
    while let Some(entry) = dir_iter.next_entry().await? {
        let file_type = entry.file_type().await?;
        let entry_path = entry.path();
        let entry_path: &Utf8Path = entry_path.as_path().try_into()?;
        let entry_file_name = entry_path.file_name().context("missing file name")?;

        if !file_type.is_dir() {
            continue;
        }

        if let Some(filter_regex) = filter_regex.as_ref() {
            if !filter_regex.is_match(entry_file_name) {
                continue;
            }
        }

        let dir_path = input.join(entry_path);
        let config_path = dir_path.join("imgchest-sync.toml");
        let cache_path = dir_path.join(".imgchest-sync-cache.toml");

        let mut config = match crate::util::try_read_to_string(&config_path)
            .await
            .context("failed to read config file")?
        {
            Some(config_raw) => Config::new(&config_raw).context("failed to parse config file")?,
            None => continue,
        };

        println!("syncing \"{entry_file_name}\"");

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
                            eprintln!("  {error:?}");
                            None
                        }
                    }
                }
                None => None,
            };
        }

        let mut post_config = config.post_mut();

        let mut new_post = create_post_from_post_config(&dir_path, &post_config).await?;

        let mut no_changes = false;
        match post_config.id() {
            Some(id) => {
                let online_post;
                let old_post = match cache.as_ref() {
                    Some(cache) => &cache.post,
                    None => {
                        let post = create_post_from_online(&client, id)
                            .await
                            .context("failed to create post from online")?;

                        online_post = post;
                        &online_post
                    }
                };

                let diffs = generate_post_diffs(old_post, &new_post)
                    .context("failed to generate post diffs")?;
                let diff_empty = diffs
                    .iter()
                    .all(|diff| matches!(diff, PostDiff::RetainFile { .. }));

                if options.print_diffs {
                    println!("  diffs: [");
                    for diff in diffs.iter() {
                        println!("    {diff:?},");
                    }
                    println!("  ]");
                }

                if !diff_empty {
                    println!("  updating post");
                    update_online_post(&client, id, diffs, old_post, &mut new_post, &cache_path)
                        .await?;
                } else {
                    println!("  no changes");

                    // Copy file ids
                    for (new_file, old_file) in new_post.files.iter_mut().zip(old_post.files.iter())
                    {
                        let id = old_file.id.as_ref().context("missing old id")?.clone();
                        new_file.id = Some(id);
                    }

                    no_changes = true;
                }
            }
            None => {
                let mut builder = imgchest::CreatePostBuilder::new();
                builder
                    .title(new_post.title.clone())
                    .privacy(match new_post.privacy {
                        PostPrivacy::Public => imgchest::PostPrivacy::Public,
                        PostPrivacy::Hidden => imgchest::PostPrivacy::Hidden,
                        PostPrivacy::Secret => imgchest::PostPrivacy::Secret,
                    })
                    .nsfw(new_post.nsfw);

                // imgchest only supports uploading 20 images at once for normal users.
                let first_20_chunk = new_post
                    .files
                    .chunks(20)
                    .next()
                    .context("missing first 20 images chunk")?;
                for file in first_20_chunk {
                    let path = file.path.as_ref().context("missing path")?;
                    let file = imgchest::UploadPostFile::from_path(&path)
                        .await
                        .with_context(|| format!("failed to open image at \"{path}\""))?;

                    builder.image(file);
                }

                println!("  creating new post");
                let mut imgchest_post = client
                    .create_post(builder)
                    .await
                    .context("failed to create new post")?;
                post_config.set_id(Some(&*imgchest_post.id));

                // Upload remaining images if we couldn't do it all upfront.
                if new_post.files.len() > 20 {
                    // We should have already uploaded the first 20.
                    for chunk in new_post.files.chunks(20).skip(1) {
                        let mut files = Vec::with_capacity(chunk.len());
                        for file in chunk.iter() {
                            let path = file.path.as_ref().context("missing path")?;
                            let file = imgchest::UploadPostFile::from_path(&path)
                                .await
                                .with_context(|| format!("failed to open image at \"{path}\""))?;
                            files.push(file);
                        }
                        imgchest_post = client.add_post_images(&imgchest_post.id, files).await?;
                    }
                }

                // Set descriptions
                ensure!(new_post.files.len() == imgchest_post.images.len());
                let description_updates: Vec<_> = new_post
                    .files
                    .iter()
                    .zip(imgchest_post.images.iter())
                    .filter(|(file, _new_file)| !file.description.is_empty())
                    .map(|(file, new_file)| imgchest::FileUpdate {
                        id: new_file.id.to_string(),
                        description: file.description.clone(),
                    })
                    .collect();
                if !description_updates.is_empty() {
                    client
                        .update_files_bulk(description_updates)
                        .await
                        .context("failed to set file descriptions")?;
                }

                ensure!(imgchest_post.images.len() == new_post.files.len());
                for (file, imgchest_image) in new_post
                    .files
                    .iter_mut()
                    .zip(Vec::from(imgchest_post.images).into_iter())
                {
                    file.id = Some(imgchest_image.id.into());
                }

                crate::util::write_string_safe(config_path, &config.to_string())
                    .await
                    .context("failed to write new config")?;
            }
        }

        if !(cache.is_some() && no_changes) {
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

            crate::util::write_string_safe(cache_path, &cache_str)
                .await
                .context("failed to write new cache")?;
        }
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

        let mut futures = Vec::with_capacity(files_config.len());
        for file in files_config.iter() {
            let (tx, rx) = tokio::sync::oneshot::channel();

            let description: String = file.description().unwrap_or("").into();

            let path = Utf8Path::new(file.path());
            let path: Utf8PathBuf = if path.is_relative() {
                dir_path.join(path)
            } else {
                path.into()
            };

            rayon::spawn(move || {
                let sha256_result = hash_file_at_path(&path)
                    .with_context(|| format!("failed to hash file at \"{path}\""));
                let result = sha256_result.map(|sha256| PostFile {
                    description,
                    sha256,
                    path: Some(path),
                    id: None,
                });

                let _ = tx.send(result).is_ok();
            });

            futures.push(rx);
        }

        let mut files = Vec::with_capacity(files_config.len());
        for future in futures {
            let file = future.await??;
            files.push(file);
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

fn hash_file_at_path(path: &Utf8Path) -> anyhow::Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("failed to open \"{path}\""))?;

    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let hash = hasher.finalize();
    let hex_hash = base16ct::lower::encode_string(&hash);

    anyhow::Ok(hex_hash)
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
            let description = image
                .description
                .map(String::from)
                .unwrap_or_else(String::new);

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
                description,
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
    let mut file_updates = Vec::new();
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
            PostDiff::EditFileDescription { index, description } => {
                let id = old_post.files[index]
                    .id
                    .as_ref()
                    .context("old post missing id")?
                    .clone();
                file_updates.push(imgchest::FileUpdate { id, description });
            }
            PostDiff::RetainFile { index } => {
                let id = old_post.files[index]
                    .id
                    .as_ref()
                    .context("old post missing id")?
                    .clone();
                new_post.files[index].id = Some(id);
            }
            PostDiff::AddFile { index } => {
                let path = new_post.files[index]
                    .path
                    .as_ref()
                    .context("missing path")?;
                let file = imgchest::UploadPostFile::from_path(path)
                    .await
                    .with_context(|| format!("failed to open \"{path}\" for upload"))?;
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
    match tokio::fs::remove_file(&cache_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("failed to remove cache file");
        }
    }

    if let Some(update_post_builder) = update_post_builder {
        client.update_post(id, update_post_builder).await?;
    }

    if !files_to_add.is_empty() {
        let mut imgchest_post = None;
        let mut files_to_add_iter = files_to_add.into_iter();
        while !files_to_add_iter.as_slice().is_empty() {
            imgchest_post = Some(
                client
                    .add_post_images(id, files_to_add_iter.by_ref().take(20))
                    .await?,
            );
        }
        let imgchest_post = imgchest_post.expect("imgchest_post should be populated");
        for (i, file_index) in files_to_add_indicies.into_iter().enumerate() {
            let imgchest_image = &imgchest_post.images[old_post.files.len() + i];
            let new_post_file = &mut new_post.files[file_index];

            let id = String::from(imgchest_image.id.clone());
            let description = &new_post_file.description;

            new_post_file.id = Some(id.clone());

            // If the new description is empty,
            // do nothing.
            // We just created a new post,
            // so it must be empty already.
            if !description.is_empty() {
                file_updates.push(imgchest::FileUpdate {
                    id,
                    description: description.clone(),
                });
            }
        }
    }

    // This needs to happen after we add our files, in case the post is empied.
    for id in files_to_remove.iter() {
        client.delete_file(id).await?;
    }

    if !file_updates.is_empty() {
        client.update_files_bulk(file_updates).await?;
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
        // TODO: It is possible to keep searching after a mismatch here
        // if we can make the file sequence match again by only deleting from the old post.
        if old_file.sha256 != new_file.sha256 {
            break;
        }

        let mut edit_description = false;
        if old_file.description != new_file.description {
            // We know that the description needs an update.
            // However, the API does not allow clearing a description.
            // In this case, we are forced to recreate the file.
            // As a result, we are forced to end our same file prefix search.
            if new_file.description.is_empty() {
                break;
            } else {
                edit_description = true;
            }
        }

        diffs.push(PostDiff::RetainFile {
            index: prefix_index,
        });

        if edit_description {
            diffs.push(PostDiff::EditFileDescription {
                index: prefix_index,
                description: new_file.description.clone(),
            });
        }

        prefix_index += 1;
    }

    for index in prefix_index..new.files.len() {
        // Since we removed all the posts with the earlier diff,
        // The current old post object is a prefix of the new post object.
        // Therefore, the indicies of the new post work with the old one.
        diffs.push(PostDiff::AddFile { index });
    }

    for index in prefix_index..old.files.len() {
        diffs.push(PostDiff::RemoveFile { index });
    }

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
            PostDiff::AddFile { index: 0 },
            PostDiff::RemoveFile { index: 0 },
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
                description: "hello world!".into(),
                sha256: SHA256_A.into(),
                id: None,
                path: None,
            }],
        };
        let actual_diffs =
            generate_post_diffs(&old_post, &new_post).expect("failed to generate diffs");
        let expected_diffs = vec![
            PostDiff::RetainFile { index: 0 },
            PostDiff::EditFileDescription {
                index: 0,
                description: "hello world!".into(),
            },
        ];
        dbg!(&actual_diffs);
        assert!(actual_diffs == expected_diffs);
    }
}
