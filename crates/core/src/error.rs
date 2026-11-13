use thiserror::Error;

/// Engine errors. The UI maps each to a sentence through [`Error::kind`].
#[derive(Debug, Error)]
pub enum Error {
    /// The data did not authenticate: wrong key, wrong role, or corruption.
    #[error("data failed authentication: {0}")]
    Authentication(&'static str),
    /// The password or recovery key does not open this repository.
    #[error("wrong password or recovery key")]
    WrongKey,
    /// The bytes are not what the format says they should be.
    #[error("format error: {0}")]
    Format(String),
    /// An object the repository needs is not in the destination.
    #[error("not found in the backup: {0}")]
    NotFound(String),
    /// A blob the index does not know, or whose pack is gone.
    #[error("the backup is missing a piece of data ({0}…)")]
    MissingBlob(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// A stable string the UI turns into an actionable sentence.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Authentication(_) => "corrupt",
            Error::WrongKey => "wrong-key",
            Error::Format(_) => "format",
            Error::NotFound(_) => "not-found",
            Error::MissingBlob(_) => "missing-blob",
            Error::Io(_) => "io",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
