//! Checked input, arithmetic and cancellation errors.
use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidInput(String),
    Overflow(String),
    NotSolved(&'static str),
    Cancelled,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(s) => write!(f, "invalid input: {s}"),
            Self::Overflow(s) => write!(f, "overflow: {s}"),
            Self::NotSolved(s) => write!(f, "not solved yet: call {s} first"),
            Self::Cancelled => f.write_str("solve cancelled"),
        }
    }
}
impl std::error::Error for Error {}
pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(message.into())
}
