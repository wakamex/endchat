use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Msg(String),
    #[error("profile is locked by another process")]
    Locked,
    #[error("{file} changed outside this process: expected {expected} bytes, found {found}")]
    ForeignWrite { file: String, expected: u64, found: u64 },
    #[error("index disagrees with the log at record {0}")]
    IndexMismatch(u64),
    #[error("aop: {0}")]
    Aop(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn msg<T>(s: impl Into<String>) -> Result<T> {
    Err(Error::Msg(s.into()))
}
