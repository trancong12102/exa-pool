//! Crate-wide error type with stable process exit codes.

use std::fmt;

/// Every failure the CLI can report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Configuration file or environment is unusable.
    Config(String),
    /// State file could not be read, locked, or written.
    State(String),
    /// Caller supplied bad input (not forwarded to Exa).
    Input(String),
    /// Exa rejected the request itself; rotating keys would not help.
    Request {
        /// HTTP status returned by Exa.
        status: u16,
        /// Exa error tag, when the body carried one.
        tag: Option<String>,
        /// Human-readable message from Exa.
        message: String,
    },
    /// Every key is exhausted, invalid, or cooling down for too long.
    NoUsableKeys(String),
    /// Transient failures persisted past the attempt budget.
    Upstream {
        /// Attempts made before giving up.
        attempts: u32,
        /// Last error observed.
        last: String,
    },
}

impl Error {
    /// Process exit code for this error.
    ///
    /// - `1` generic (config, state, input)
    /// - `3` Exa rejected the request (4xx that is not key-related)
    /// - `4` no usable key left in the pool
    /// - `5` upstream kept failing
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Config(_) | Self::State(_) | Self::Input(_) => 1,
            Self::Request { .. } => 3,
            Self::NoUsableKeys(_) => 4,
            Self::Upstream { .. } => 5,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "config: {msg}"),
            Self::State(msg) => write!(f, "state: {msg}"),
            Self::Input(msg) => write!(f, "input: {msg}"),
            Self::Request {
                status,
                tag,
                message,
            } => match tag {
                Some(tag) => write!(
                    f,
                    "exa rejected request ({status} {tag}): {message}; fix the arguments, retrying will not help"
                ),
                None => write!(
                    f,
                    "exa rejected request ({status}): {message}; fix the arguments, retrying will not help"
                ),
            },
            Self::NoUsableKeys(msg) => write!(f, "no usable api key: {msg}"),
            Self::Upstream { attempts, last } => write!(
                f,
                "exa unavailable after {attempts} attempt(s): {last}; retry later"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;
