use std::fmt;

/// The error returned by the crate's public operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The caller set the cancellation flag before the operation finished.
    Cancelled,
    /// The operation failed. The message is suitable for showing to a user.
    Failed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => formatter.write_str("Operation cancelled."),
            Error::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Error::Failed(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Error::Failed(message.to_owned())
    }
}
