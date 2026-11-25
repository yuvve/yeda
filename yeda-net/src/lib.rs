//! Network I/O shared by every yeda client: the magic wand's metadata lookups, LLM requests and
//! WebDAV, Dropbox and Google Drive attachment storage, and citation data from OpenAlex.

pub mod llm;
pub mod wand;
pub mod web_search;
pub mod webdav;
pub mod remote;
pub mod dropbox;
pub mod openalex;
pub mod fulltext;
pub mod codex;
pub mod oauth;

/// The most a downloaded file may take in memory; a server that sends more is refused.
pub const MAX_BYTES: u64 = 200 << 20;
