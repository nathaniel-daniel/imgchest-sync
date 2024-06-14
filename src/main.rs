mod config;
mod util;

use crate::config::Config;
use crate::config::PostConfig;
use anyhow::ensure;
use anyhow::Context;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use sha2::Digest;
use sha2::Sha256;
use std::collections::VecDeque;

/// Representation of a post.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Post {
    /// The title
    pub title: String,

    /// The privacy of this post.
    pub privacy: PostPrivacy,

    /// Whether this post is nsfw.
    pub nsfw: bool,

    /// The post files
    pub files: Vec<PostFile>,
}

/// The post privacy.
#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum PostPrivacy {
    #[serde(rename = "public")]
    Public,
    #[serde(rename = "hidden")]
    Hidden,
    #[serde(rename = "secret")]
    Secret,
}

/// A post image
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct PostFile {
    /// The post file description.
    pub description: String,

    /// The sha256 file hash, as a hex string.
    pub sha256: String,

    /// The post path.
    ///
    /// This may not exist in certain cases,
    /// like loading a cache from the disk.
    ///
    /// This should not be used when diffing.
    pub path: Option<Utf8PathBuf>,

    /// The post id
    ///
    /// This may not exist in certain cases,
    /// like creating a post from a config file.
    ///
    /// This should not be used when diffing.
    pub id: Option<String>,
}

/// A diff for a post.
#[derive(Debug)]
pub enum PostDiff {
    EditTitle {
        /// The new title
        title: String,
    },
    EditPrivacy {
        /// The new privacy setting
        privacy: PostPrivacy,
    },
    EditNsfw {
        /// The new nsfw setting
        nsfw: bool,
    },
    AddFile {
        /// The index of the new post.
        index: usize,

        /// The sha256 file hash, as a hex string.
        sha256: String,
    },
    RemoveFile {
        /// The index of the file to remove
        index: usize,
    },
}

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

        let mut cache = match crate::util::try_read_to_string(&cache_path)
            .await
            .context("failed to read cache file")?
        {
            Some(cache_raw) => {
                match toml::from_str::<Cache>(&cache_raw).context("failed to parse cache file") {
                    Ok(cache) => Some(cache),
                    Err(error) => {
                        eprintln!("{error:?}");
                        None
                    }
                }
            }
            None => None,
        };

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
                let mut update_post_builder = imgchest::UpdatePostBuilder::new();
                let mut files_to_remove = Vec::new();
                for diff in diffs {
                    match diff {
                        PostDiff::EditTitle { title } => {
                            update_post_builder.title(title);
                        }
                        PostDiff::EditPrivacy { privacy } => {
                            update_post_builder.privacy(match privacy {
                                PostPrivacy::Public => imgchest::PostPrivacy::Public,
                                PostPrivacy::Hidden => imgchest::PostPrivacy::Hidden,
                                PostPrivacy::Secret => imgchest::PostPrivacy::Secret,
                            });
                        }
                        PostDiff::EditNsfw { nsfw } => {
                            update_post_builder.nsfw(nsfw);
                        }
                        PostDiff::AddFile { index, sha256 } => {
                            todo!("add file {sha256} at {index}");
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

                client.update_post(id, update_post_builder).await?;
                for id in files_to_remove.iter() {
                    client.delete_file(id).await?;
                }

                todo!("inject file ids into new_post");
                // New files should have their ids updated on creation.
                // Old files should copy their ids from the old post.
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
    // TODO: Create and map config model.
    let privacy = PostPrivacy::Hidden;
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

fn generate_post_diffs(old: &Post, new: &Post) -> anyhow::Result<Vec<PostDiff>> {
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

    let old_files = &old.files;
    let new_files = &new.files;
    let old_files_len = old_files.len();
    let new_files_len = new_files.len();
    ensure!(
        old_files_len < u16::MAX.into(),
        "too many files in old post"
    );
    ensure!(
        new_files_len < u16::MAX.into(),
        "too many files in new post"
    );

    let mut lcs: Vec<Vec<u16>> = vec![vec![0; new_files_len + 1]; old_files_len + 1];
    for i in 1..(old_files_len + 1) {
        for j in 1..(new_files_len + 1) {
            if old_files[i - 1].sha256 == new_files[j - 1].sha256 {
                lcs[i][j] = lcs[i - 1][j - 1] + 1;
            } else {
                lcs[i][j] = std::cmp::max(lcs[i - 1][j], lcs[i][j - 1]);
            }
        }
    }

    let mut sequence = VecDeque::new();
    let mut i = old_files_len;
    let mut j = new_files_len;
    loop {
        if i == 0 || j == 0 {
            break;
        } else if old_files[i - 1].sha256 == new_files[j - 1].sha256 {
            sequence.push_back(i - 1);

            i -= 1;
            j -= 1;
        } else if lcs[i][j - 1] > lcs[i - 1][j] {
            j -= 1;
        } else {
            i -= 1;
        }
    }

    let mut i = 0;
    let mut j = 0;
    loop {
        let old_file = old_files.get(i);
        let new_file = new_files.get(j);

        let old_file = match old_file {
            Some(old_file) => old_file,
            None => {
                for index in j..new_files_len {
                    diffs.push(PostDiff::AddFile {
                        index,
                        sha256: new_files[j].sha256.clone(),
                    });
                }
                break;
            }
        };

        let new_file = match new_file {
            Some(new_file) => new_file,
            None => {
                for index in i..old_files_len {
                    diffs.push(PostDiff::RemoveFile { index });
                }
                break;
            }
        };

        if old_file.sha256 == new_file.sha256 {
            i += 1;
            j += 1;
        } else {
            todo!("wip");
        }
    }

    anyhow::Ok(diffs)
}
