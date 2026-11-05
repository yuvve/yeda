//! The yeda library format and business logic. Callers do all disk and network I/O: functions take
//! file contents and return values or [`Effect`]s for the caller to apply.

pub mod annotations;
pub mod browse;
pub mod clusters;
pub mod discover;
pub mod cite;
pub mod fulltext;
pub mod graph;
pub mod ids;
pub mod library;
pub mod llm;
pub mod search;
pub mod tables;
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

/// Refuses a server that would get a password or key in the clear: anything but `https://`,
/// except plain `http://` to this machine (a local LLM server, say).
pub fn require_https(url: &str) -> anyhow::Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => host.split(':').next().unwrap_or_default(),
        };
        if host.eq_ignore_ascii_case("localhost") || host == "::1" || host.starts_with("127.") {
            return Ok(());
        }
    }
    anyhow::bail!("{url}: passwords and keys only go over https:// (plain http:// only to localhost)")
}

#[cfg(test)]
mod tests {
    use super::require_https;

    #[test]
    fn only_https_or_local_http_carries_secrets() {
        for ok in ["https://dav.example.com/pdfs", "http://localhost:11434/v1", "http://127.0.0.1/", "http://[::1]:8080/v1", "http://me:pw@localhost/x"] {
            assert!(require_https(ok).is_ok(), "{ok}");
        }
        for bad in ["http://dav.example.com/pdfs", "http://localhost.evil.com/", "http://me@evil.com:80/", "http://[2001:db8::1]/", "ftp://x", "dav.example.com", ""] {
            assert!(require_https(bad).is_err(), "{bad}");
        }
    }
}
