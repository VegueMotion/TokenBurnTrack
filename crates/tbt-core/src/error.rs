use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error{}: {source}", path_suffix(.path))]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    #[error("unsupported source format: {0}")]
    UnsupportedFormat(String),
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: Some(path.into()),
            source,
        }
    }

    pub fn other(err: impl Into<Box<dyn std::error::Error + Send + Sync + 'static>>) -> Self {
        Error::Other(err.into())
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { path: None, source }
    }
}

fn path_suffix(path: &Option<PathBuf>) -> String {
    match path {
        Some(p) => format!(" at {}", p.display()),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_display_includes_path() {
        let e = Error::io("/tmp/x.jsonl", std::io::Error::other("boom"));
        assert_eq!(e.to_string(), "I/O error at /tmp/x.jsonl: boom");
        let e: Error = std::io::Error::other("boom").into();
        assert_eq!(e.to_string(), "I/O error: boom");
    }

    #[test]
    fn other_wraps_strings() {
        let e = Error::other("custom failure");
        assert_eq!(e.to_string(), "custom failure");
    }
}
