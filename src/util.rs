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
