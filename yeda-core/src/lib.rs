//! The yeda library format and business logic. Callers do all disk and network I/O: functions take
//! file contents and return values or [`Effect`]s for the caller to apply.

pub mod annotations;
pub mod assist;
pub mod browse;
pub mod clusters;
pub mod critic;
pub mod discover;
pub mod cite;
pub mod fulltext;
pub mod graph;
pub mod ids;
pub mod library;
pub mod llm;
pub mod search;
pub mod tables;
pub mod web_search;
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

/// Refuses a server that would get a password or key in the clear: anything but `https://`.
pub fn require_https(url: &str) -> anyhow::Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    anyhow::bail!("{url}: passwords and keys only go over https://")
}

/// The part of an environment variable's name that identifies the server of `url`: its host
/// uppercased, `.` as `_` and `-` as `__`, then `_<port>` (443 if the URL has none). The port is
/// always there so that a host's last label cannot pass for one. `None` where the name would not
/// identify one server: no `https://`, userinfo, an IPv6 address, or a label that is empty or
/// starts or ends with `-`.
pub fn env_host(url: &str) -> Option<String> {
    let authority = url.strip_prefix("https://")?.split(['/', '?', '#']).next()?;
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => (host, port),
        Some(_) => return None,
        None => (authority, "443"),
    };
    let label = |l: &str| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !host.split('.').all(label) {
        return None;
    }
    Some(format!("{}_{port}", host.to_ascii_uppercase().replace('.', "_").replace('-', "__")))
}

/// Refuses an alias that cannot be part of an environment variable's name.
pub fn check_alias(alias: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!alias.is_empty() && alias.bytes().all(|b| b.is_ascii_alphanumeric()), "alias '{alias}': use only letters and digits");
    Ok(())
}

/// Refuses an alias used twice for one server, since both would read the same variable.
pub fn check_aliases<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str, Option<&'a str>)>) -> anyhow::Result<()> {
    let mut seen = std::collections::HashMap::new();
    for (name, url, alias) in entries {
        let Some(alias) = alias else { continue };
        check_alias(alias).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        let Some(host) = env_host(url) else { continue };
        if let Some(other) = seen.insert((host, alias.to_ascii_uppercase()), name) {
            anyhow::bail!("{name} and {other} both use alias '{alias}' on {url}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check_aliases, env_host, require_https};

    #[test]
    fn env_host_names_one_server() {
        assert_eq!(env_host("https://api.anthropic.com/v1").as_deref(), Some("API_ANTHROPIC_COM_443"));
        assert_eq!(env_host("https://api.anthropic.com:443").as_deref(), Some("API_ANTHROPIC_COM_443"));
        assert_eq!(env_host("https://my-llm.lan:8443").as_deref(), Some("MY__LLM_LAN_8443"));
        assert_eq!(env_host("https://10.0.0.1").as_deref(), Some("10_0_0_1_443"));
        assert_ne!(env_host("https://h.80"), env_host("https://h:80"));
        for bad in ["http://a.example", "https://u@a.example", "https://[::1]:1", "https://a..b", "https://h.", "https://-a.b", "https://a-.b", "https://a.example:"] {
            assert_eq!(env_host(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_alias_is_unique_per_server() {
        let a = "https://api.example.com/v1";
        let other = "https://other.example.com";
        assert!(check_aliases([("a", a, Some("work")), ("b", other, Some("Work")), ("c", a, None)]).is_ok());
        assert!(check_aliases([("a", a, Some("work")), ("b", "https://api.example.com/v2", Some("WORK"))]).is_err());
        assert!(check_aliases([("a", a, Some("my-work"))]).is_err());
    }

    #[test]
    fn only_https_carries_secrets() {
        for ok in ["https://dav.example.com/pdfs", "https://localhost:11434/v1", "https://desktop:11434/v1"] {
            assert!(require_https(ok).is_ok(), "{ok}");
        }
        for bad in ["http://dav.example.com/pdfs", "http://localhost:11434/v1", "http://127.0.0.1/", "http://[::1]:8080/v1", "http://me:pw@localhost/x", "http://desktop:11434/v1", "HTTPS://dav.example.com/", "http://localhost.evil.com/", "http://me@evil.com:80/", "http://[2001:db8::1]/", "ftp://x", "dav.example.com", "", "http:///", "http://2130706433/", "http://0x7f000001/", "http://[fe80::1]/"] {
            assert!(require_https(bad).is_err(), "{bad}");
        }
    }
}
