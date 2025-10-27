use anyhow::Context;
use camino::Utf8Path;
use sha2::Digest;
use sha2::Sha256;
use std::path::Path;

/// Try to read a string from a path, if it exists.
pub async fn try_read_to_string(path: impl AsRef<Path>) -> std::io::Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => Ok(Some(s)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Write a string to the given path, using a temp file.
pub async fn write_string_safe<P>(path: P, data: &str) -> anyhow::Result<()>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let tmp_path = nd_util::with_push_extension(path, "temp");
    tokio::fs::write(&tmp_path, data).await?;
    tokio::fs::rename(tmp_path, path).await?;

    Ok(())
}

/// Add images from a vec to a post by id, batching so it can handle arbitrary sizes.
pub async fn add_post_images_batched(
    client: &imgchest::Client,
    id: &str,
    images: Vec<imgchest::UploadPostFile>,
    batch_size: usize,
) -> anyhow::Result<imgchest::Post> {
    let mut imgchest_post = None;
    let mut images = images.into_iter();
    while !images.as_slice().is_empty() {
        imgchest_post = Some(
            client
                .add_post_images(id, images.by_ref().take(batch_size))
                .await?,
        );
    }
    imgchest_post.context("missing imgchest post")
}

/// Hash a file ath the given path, getting the result as a hex string.
pub fn hash_file_at_path(path: &Utf8Path) -> anyhow::Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("failed to open \"{path}\""))?;

    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let hash = hasher.finalize();
    let hex_hash = base16ct::lower::encode_string(&hash);

    anyhow::Ok(hex_hash)
}
