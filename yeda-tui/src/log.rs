use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1 << 20;

/// Status messages with a timestamp, in `$XDG_STATE_HOME/yeda/yeda.log`; the previous megabyte is kept as `yeda.log.1`.
pub struct Log {
    pub path: Option<PathBuf>,
    last: String,
}

impl Log {
    pub fn open() -> Self {
        let path = dirs::state_dir().or_else(dirs::cache_dir).map(|d| d.join("yeda/yeda.log"));
        if let Some(p) = &path {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            if std::fs::metadata(p).is_ok_and(|m| m.len() > MAX_BYTES) {
                let _ = std::fs::rename(p, p.with_extension("log.1"));
            }
        }
        Log { path, last: String::new() }
    }

    /// Appends `line` unless it is empty or the same as the last one.
    pub fn write(&mut self, line: &str) {
        if line.is_empty() || line == self.last {
            return;
        }
        self.last = line.to_string();
        let Some(path) = &self.path else { return };
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let entry = format!("{} {}\n", stamp(secs), redact(line));
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(entry.as_bytes()));
    }
}

/// `line` without API keys in query strings or passwords in URLs.
fn redact(line: &str) -> String {
    let mut out = String::new();
    for word in line.split_inclusive(char::is_whitespace) {
        let word = match word.find("api_key=") {
            Some(i) => {
                let end = word[i..].find(['&', ' ', '\n']).map_or(word.len(), |j| i + j);
                format!("{}api_key=…{}", &word[..i], &word[end..])
            }
            None => word.to_string(),
        };
        let word = match (word.find("://"), word.find('@')) {
            (Some(s), Some(a)) if a > s && !word[s + 3..a].contains('/') => format!("{}://…{}", &word[..s], &word[a..]),
            _ => word,
        };
        out.push_str(&word);
    }
    out
}

/// `2026-10-17T19:05:03Z` for seconds since the epoch.
fn stamp(secs: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let (days, rem) = ((secs / 86_400) as i64 + 719_468, secs % 86_400);
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn stamps_and_redacts() {
        assert_eq!(super::stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::stamp(1_792_263_903), "2026-10-17T19:05:03Z");
        assert_eq!(super::stamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::redact("GET https://x.org/w?api_key=abc&page=2 failed"), "GET https://x.org/w?api_key=…&page=2 failed");
        assert_eq!(super::redact("push https://me:tok@github.com/a/b"), "push https://…@github.com/a/b");
        assert_eq!(super::redact("see https://doi.org/10.1/x@y"), "see https://doi.org/10.1/x@y");
    }
}
