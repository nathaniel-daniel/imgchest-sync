/// Read a string from a path.
use std::path::Path;

/// Try to read a string from a path, if it exists.
pub async fn try_read_to_string(path: impl AsRef<Path>) -> std::io::Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => Ok(Some(s)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
