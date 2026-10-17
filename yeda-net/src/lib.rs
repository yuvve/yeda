//! Network I/O shared by every yeda client: the magic wand's metadata lookups, LLM requests and
//! WebDAV attachment storage, and citation data from OpenAlex.

pub mod llm;
pub mod wand;
pub mod webdav;
pub mod openalex;
