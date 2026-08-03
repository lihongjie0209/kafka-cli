//! Application error types and stable process exit classification.

use std::io;

/// Result type used by the library.
pub type Result<T> = std::result::Result<T, Error>;

/// Failures returned by command parsing, configuration, Kafka, and I/O operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An invocation or input file is invalid.
    #[error("{0}")]
    Usage(String),
    /// Client configuration is invalid.
    #[error("configuration error: {0}")]
    Config(String),
    /// Kafka returned an error.
    #[error(transparent)]
    Kafka(#[from] rdkafka::error::KafkaError),
    /// The pure-Rust Kafka protocol client returned an error.
    #[error(transparent)]
    Krafka(#[from] krafka::KrafkaError),
    /// A local I/O operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// JSON input or output failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The broker does not support the requested operation.
    #[error("unsupported operation: {0}")]
    Unsupported(String),
    /// One or more members of a batch operation failed.
    #[error("{failed} of {total} operations failed")]
    Partial { failed: usize, total: usize },
}

impl Error {
    /// Returns the stable process exit code for this error.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::Config(_) | Self::Json(_) => 2,
            Self::Kafka(_)
            | Self::Krafka(_)
            | Self::Io(_)
            | Self::Unsupported(_)
            | Self::Partial { .. } => 1,
        }
    }
}
