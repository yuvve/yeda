#[derive(Debug, PartialEq)]
pub enum Id {
    Doi(String),
    Arxiv(String),
    Isbn(String),
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

fn strip_version(id: &str) -> &str {
    match id.rsplit_once('v') {
        Some((base, v)) if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) => base,
        _ => id,
    }
}

fn is_arxiv(id: &str) -> bool {
    let digits = |s: &str, lens: &[usize]| lens.contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit());
    match (id.split_once('.'), id.split_once('/')) {
        (Some((a, b)), None) => digits(a, &[4]) && digits(b, &[4, 5]),
        (_, Some((archive, n))) => {
            !archive.is_empty()
                && archive.chars().all(|c| c.is_ascii_lowercase() || c == '-' || c == '.')
                && digits(n, &[7])
        }
        _ => false,
    }
}

pub fn parse_id(input: &str) -> Option<Id> {
    let s = input.trim();
    for p in ["https://doi.org/", "http://doi.org/", "https://dx.doi.org/", "doi.org/", "doi:"] {
        if let Some(rest) = strip_prefix_ci(s, p) {
            return Some(Id::Doi(rest.trim().into()));
        }
    }
    for p in ["https://arxiv.org/abs/", "https://arxiv.org/pdf/", "arxiv.org/abs/", "arxiv:"] {
        if let Some(rest) = strip_prefix_ci(s, p) {
            return Some(Id::Arxiv(strip_version(rest.trim().trim_end_matches(".pdf")).into()));
        }
    }
    if s.starts_with("10.") && s.contains('/') {
        return Some(Id::Doi(s.into()));
    }
    if is_arxiv(strip_version(s)) {
        return Some(Id::Arxiv(strip_version(s).into()));
    }
    let isbn: String = strip_prefix_ci(s, "isbn")
        .unwrap_or(s)
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | ':'))
        .collect::<String>()
        .to_uppercase();
    let valid = match isbn.len() {
        10 => isbn[..9].chars().all(|c| c.is_ascii_digit()) && isbn.ends_with(|c: char| c.is_ascii_digit() || c == 'X'),
        13 => isbn.chars().all(|c| c.is_ascii_digit()),
        _ => false,
    };
    valid.then_some(Id::Isbn(isbn))
}

/// An 8-character key in Zotero's format, so a Zotero export can keep it. Derived from `seed`
/// rather than random, so extracting again gives the same ids; FNV-1a because std's hasher may
/// change between Rust versions.
pub fn zotero_key(seed: &str) -> String {
    const ALPHABET: &[u8] = b"23456789ABCDEFGHIJKLMNPQRSTUVWXYZ";
    let mut h = seed.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    (0..8)
        .map(|_| {
            let c = ALPHABET[(h % ALPHABET.len() as u64) as usize] as char;
            h /= ALPHABET.len() as u64;
            c
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_identifiers() {
        let doi = |s: &str| Some(Id::Doi(s.into()));
        let arxiv = |s: &str| Some(Id::Arxiv(s.into()));
        assert_eq!(parse_id("10.1038/nature14539"), doi("10.1038/nature14539"));
        assert_eq!(parse_id("https://doi.org/10.1038/x"), doi("10.1038/x"));
        assert_eq!(parse_id("DOI: 10.1/y"), doi("10.1/y"));
        assert_eq!(parse_id("1706.03762v5"), arxiv("1706.03762"));
        assert_eq!(parse_id("arXiv:1706.03762"), arxiv("1706.03762"));
        assert_eq!(parse_id("https://arxiv.org/pdf/2101.00001v2.pdf"), arxiv("2101.00001"));
        assert_eq!(parse_id("hep-th/9901001"), arxiv("hep-th/9901001"));
        assert_eq!(parse_id("978-0-262-03384-8"), Some(Id::Isbn("9780262033848".into())));
        assert_eq!(parse_id("ISBN 0-306-40615-x"), Some(Id::Isbn("030640615X".into())));
        assert_eq!(parse_id("hello world"), None);
        assert_eq!(parse_id("12345"), None);
    }
}
