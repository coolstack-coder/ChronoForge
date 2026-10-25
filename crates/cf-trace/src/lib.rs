//! Shared vocabulary for ChronoForge: values, locations, step events, the
//! nondeterminism log, the `.cfrec` file format and storage backends.

pub mod event;
pub mod format;
pub mod native;
pub mod store;
pub mod value;

pub use event::*;
pub use value::{Loc, Val};

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("encoding: {0}")]
    Encoding(#[from] bincode::Error),
    #[error("corrupt trace: {0}")]
    Corrupt(String),
    #[error("no such recording: {0}")]
    NotFound(String),
    #[error("storage backend: {0}")]
    Backend(String),
}
