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
    /// XkyHold-lx only reads version 1 repositories (FORMAT-v2.md 7).
    #[error("this backup was made by XkyHold for the browser; it can be restored here but not added to")]
    ReadOnly,
    /// A backup already exists where a new one was to be created.
    #[error("a backup already exists here")]
    Exists,
    /// Another device is using the backup (a fresh lock).
    #[error("another device is using this backup right now")]
    Busy,
    /// The operation was cancelled.
    #[error("cancelled")]
    Aborted,
    /// A chosen folder could not be read.
    #[error("could not read {path}: {source}")]
    Read { path: String, source: std::io::Error },
    /// XkyHold-lx never runs as root.
    #[error("XkyHold-lx does not run as root")]
    Root,
    /// A path outside the home folder, or not a valid path inside it.
    #[error("only items inside your home folder can be backed up: {0}")]
    OutsideHome(String),
    #[error("internal error: {0}")]
    Bug(&'static str),
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
            Error::ReadOnly => "read-only",
            Error::Exists => "exists",
            Error::Busy => "busy",
            Error::Aborted => "aborted",
            Error::Read { .. } => "read",
            Error::Root => "root",
            Error::OutsideHome(_) => "outside-home",
            Error::Bug(_) => "other",
            Error::Io(_) => "io",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
