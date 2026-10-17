//! The yeda library format and business logic. Callers do all disk and network I/O: functions take
//! file contents and return values or [`Effect`]s for the caller to apply.

pub mod annotations;
pub mod browse;
pub mod fulltext;
pub mod graph;
pub mod ids;
pub mod library;
pub mod llm;
pub mod search;
pub mod tables;
pub mod webdav;
#[cfg(feature = "zotero")]
pub mod zotero;

/// A change to the library on disk; paths are relative to the library root and use `/`.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Creates parent folders as needed; must be atomic, since other yeda clients write the same files.
    Write { path: String, bytes: Vec<u8> },
    /// Removes a file or a whole folder; missing is not an error.
    Remove { path: String },
}

impl Effect {
    pub fn write(path: impl Into<String>, text: impl Into<Vec<u8>>) -> Self {
        Effect::Write { path: path.into(), bytes: text.into() }
    }

    pub fn remove(path: impl Into<String>) -> Self {
        Effect::Remove { path: path.into() }
    }
}
