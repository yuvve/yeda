//! Network I/O shared by every yeda client: the magic wand's metadata lookups, LLM requests and
//! WebDAV attachment storage, and citation data from OpenAlex.

pub mod llm;
pub mod wand;
pub mod webdav;
pub mod openalex;
pub mod fulltext;

/// The most a downloaded file may take in memory; a server that sends more is refused.
pub const MAX_BYTES: u64 = 200 << 20;
