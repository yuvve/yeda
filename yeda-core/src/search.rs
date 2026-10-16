use crate::library::Ref;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

#[derive(Debug, PartialEq)]
pub struct Query {
    pub fields: Vec<(String, String)>,
    pub free: String,
}

/// Whitespace-separated tokens; `"…"` groups words. Returns each token and whether it was quoted.
fn tokens(q: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut in_quote) = (false, false);
    for c in q.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                quoted = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() || quoted {
                    out.push((std::mem::take(&mut cur), quoted));
                }
                quoted = false;
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() || quoted {
        out.push((cur, quoted));
    }
    out
}

/// Field terms are `name:value`; a value of `""` means "field is empty", a leading `!` negates.
pub fn parse(q: &str) -> Query {
    let mut fields = Vec::new();
    let mut free = Vec::new();
    for (tok, quoted) in tokens(q) {
        match tok.split_once(':') {
            Some((name, value))
                if !name.is_empty() && !value.starts_with('/') && name.chars().all(|c| c.is_ascii_alphabetic()) =>
            {
                // An unquoted empty value is a field name still being typed.
                if !value.is_empty() || quoted {
                    fields.push((name.to_lowercase(), value.to_string()));
                }
            }
            _ if quoted => free.push(escape_spaces(&tok)),
            _ => free.push(tok),
        }
    }
    Query { fields, free: free.join(" ") }
}

/// The fields a filter form offers, as `(field name, label)`.
pub const FILTER_FIELDS: [(&str, &str); 8] = [
    ("author", "Author"),
    ("title", "Title"),
    ("year", "Year"),
    ("tag", "Tag"),
    ("coll", "Collection"),
    ("status", "Status"),
    ("type", "Type"),
    ("doi", "DOI"),
];

/// Writes a query that `parse` reads back as these field terms and free text.
pub fn compose(fields: &[(String, String)], free: &str) -> String {
    let quote = |v: &str| if v.contains(char::is_whitespace) || v.is_empty() { format!("\"{v}\"") } else { v.to_string() };
    fields
        .iter()
        .map(|(name, value)| format!("{name}:{}", quote(value)))
        .chain(Some(free.trim().to_string()).filter(|f| !f.is_empty()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits on spaces that aren't escaped as `\\ ` (quoted phrases are escaped by `parse`).
fn words(free: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = free.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b' ' && (i == 0 || bytes[i - 1] != b'\\') {
            out.push(&free[start..i]);
            start = i + 1;
        }
    }
    out.push(&free[start..]);
    out.retain(|w| !w.is_empty());
    out
}

fn escape_spaces(s: &str) -> String {
    s.replace(' ', "\\ ")
}

/// Every distinct value `field` takes across `refs`, sorted.
pub fn suggestions(refs: &[Ref], field: &str) -> Vec<String> {
    let values: std::collections::BTreeSet<_> = refs.iter().flat_map(|r| field_values(r, field)).filter(|v| !v.is_empty()).collect();
    values.into_iter().collect()
}

fn field_values(r: &Ref, name: &str) -> Vec<String> {
    match name {
        "author" | "authors" => r.authors.clone(),
        "title" => vec![r.title.clone()],
        "year" => r.year.map(|y| y.to_string()).into_iter().collect(),
        "doi" => r.doi.clone().into_iter().collect(),
        "type" => vec![r.kind.clone()],
        "tag" => r.tags.clone(),
        "coll" | "collection" => r.collections.clone(),
        "key" => vec![r.key.clone()],
        "status" => r.status.clone().into_iter().collect(),
        other => r
            .extra
            .get(other)
            .map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))
            .into_iter()
            .chain(r.tables.values().filter_map(|row| row.get(other).cloned()))
            .collect(),
    }
}

pub struct Searcher {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Searcher {
    pub fn new() -> Self {
        Self { matcher: Matcher::new(Config::DEFAULT), buf: Vec::new() }
    }

    pub fn score(&mut self, needle: &str, hay: &str) -> Option<u32> {
        Pattern::parse(needle, CaseMatching::Ignore, Normalization::Smart)
            .score(Utf32Str::new(hay, &mut self.buf), &mut self.matcher)
    }

    pub fn fuzzy(&mut self, needle: &str, hay: &str) -> bool {
        self.score(needle, hay).is_some()
    }

    /// Every field term and free-text word must match (AND); within one, `|` separates
    /// alternatives of which one must match (OR).
    pub fn matches(&mut self, q: &Query, r: &Ref) -> bool {
        for (name, value) in &q.fields {
            let values = field_values(r, name);
            let any = |s: &mut Self, v: &str| v.split('|').any(|alt| values.iter().any(|x| s.fuzzy(&escape_spaces(alt), x)));
            let ok = match value.strip_prefix('!') {
                _ if value.is_empty() => values.iter().all(|v| v.is_empty()),
                Some(v) => !any(self, v),
                None => any(self, value),
            };
            if !ok {
                return false;
            }
        }
        if q.free.is_empty() {
            return true;
        }
        let year = r.year.map(|y| y.to_string()).unwrap_or_default();
        let hay = format!("{} {} {} {} {}", r.key, r.title, r.authors.join(" "), year, r.tags.join(" "));
        words(&q.free).iter().all(|w| w.split('|').any(|alt| self.fuzzy(alt, &hay)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_fields_and_free_text() {
        let q = parse("author:smith deep learning year:2020 http://x");
        assert_eq!(q.fields, vec![("author".into(), "smith".into()), ("year".into(), "2020".into())]);
        assert_eq!(q.free, "deep learning http://x");
        let q = parse(r#"status:"To Read" status:"" "big cat" author:"#);
        assert_eq!(q.fields, vec![("status".into(), "To Read".into()), ("status".into(), "".into())]);
        assert_eq!(q.free, r"big\ cat");
    }

    #[test]
    fn matches_fields_and_free_text() {
        let r = Ref {
            key: "smith2020".into(),
            title: "Deep Learning for Cats".into(),
            authors: vec!["Smith, John".into()],
            year: Some(2020),
            tags: vec!["ml".into(), "priority 1".into()],
            status: Some("To Read".into()),
            ..Default::default()
        };
        let mut s = Searcher::new();
        assert!(s.matches(&parse(r#"status:"^To Read$""#), &r));
        assert!(!s.matches(&parse(r#"status:"^Read$""#), &r));
        assert!(!s.matches(&parse(r#"status:"""#), &r));
        assert!(s.matches(&parse(r#"doi:"""#), &r));
        assert!(s.matches(&parse("tag:!'physics"), &r));
        assert!(!s.matches(&parse("tag:!'ml"), &r));
        assert!(s.matches(&parse(r#"tag:"'priority 1""#), &r));
        assert!(s.matches(&parse("author:smth dplearn"), &r));
        assert!(s.matches(&parse("tag:ml year:2020"), &r));
        assert!(!s.matches(&parse("year:2021"), &r));
        assert!(!s.matches(&parse("author:doe"), &r));
        assert!(!s.matches(&parse("quantum"), &r));
    }

    #[test]
    fn compose_round_trips_through_parse() {
        let fields = vec![("author".to_string(), "smith".to_string()), ("status".into(), "To Read".into()), ("doi".into(), "".into())];
        let q = compose(&fields, "deep learning");
        assert_eq!(q, r#"author:smith status:"To Read" doi:"" deep learning"#);
        assert_eq!(parse(&q), Query { fields, free: "deep learning".into() });
    }

    #[test]
    fn suggests_distinct_sorted_values() {
        let a = Ref { tags: vec!["ml".into(), "cats".into()], ..Default::default() };
        let b = Ref { tags: vec!["ml".into()], ..Default::default() };
        assert_eq!(suggestions(&[a, b], "tag"), ["cats", "ml"]);
    }

    #[test]
    fn alternatives_match_any() {
        let r = Ref { key: "k".into(), title: "Deep Learning for Cats".into(), tags: vec!["ml".into(), "priority 1".into()], year: Some(2020), ..Default::default() };
        let mut s = Searcher::new();
        assert!(s.matches(&parse("tag:physics|ml"), &r));
        assert!(!s.matches(&parse("tag:physics|chemistry"), &r));
        assert!(s.matches(&parse(r#"tag:"'priority 1|'priority 2""#), &r));
        assert!(!s.matches(&parse("tag:!physics|ml"), &r));
        assert!(s.matches(&parse("year:2019|2020"), &r));
        assert!(s.matches(&parse("dogs|cats learning"), &r));
        assert!(!s.matches(&parse("dogs|birds learning"), &r));
        assert!(s.matches(&parse(r#""for cats" deep"#), &r));
        assert_eq!(words(r"a b\ c  d"), ["a", r"b\ c", "d"]);
    }
}
