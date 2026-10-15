use crate::library::{Library, Ref};
use crate::search::{self, Searcher};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// An entry in the library sidebar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum Filter {
    All,
    /// A saved search: name and query.
    Search(String, String),
    Collection(String),
    Tag(String),
}

impl Filter {
    pub fn name(&self) -> &str {
        match self {
            Filter::All => "All",
            Filter::Search(name, _) => name,
            Filter::Collection(c) => c,
            Filter::Tag(t) => t,
        }
    }

    /// A collection also holds its sub-collections' references.
    pub fn accepts(&self, r: &Ref) -> bool {
        match self {
            Filter::All | Filter::Search(..) => true,
            Filter::Collection(c) => r.collections.iter().any(|x| x == c || x.starts_with(&format!("{c}/"))),
            Filter::Tag(t) => r.tags.contains(t),
        }
    }
}

/// All, the saved searches, every collection and its parents, then tags (coloured ones first).
pub fn sidebar(lib: &Library) -> Vec<Filter> {
    let mut colls = BTreeSet::new();
    let mut tags = BTreeSet::new();
    for r in &lib.refs {
        for c in &r.collections {
            let mut prefix = String::new();
            for part in c.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                colls.insert(prefix.clone());
            }
        }
        tags.extend(r.tags.iter().cloned());
    }
    let (colored, plain): (Vec<_>, Vec<_>) = tags.into_iter().partition(|t| lib.tag_colors.contains_key(t));
    std::iter::once(Filter::All)
        .chain(lib.searches.iter().map(|(n, q)| Filter::Search(n.clone(), q.clone())))
        .chain(colls.into_iter().map(Filter::Collection))
        .chain(colored.into_iter().chain(plain).map(Filter::Tag))
        .collect()
}

/// Indices into `lib.refs` that pass `filter` and match `query`.
pub fn visible(searcher: &mut Searcher, lib: &Library, filter: &Filter, query: &str) -> Vec<usize> {
    let q = search::parse(query);
    let saved = match filter {
        Filter::Search(_, s) => Some(search::parse(s)),
        _ => None,
    };
    lib.refs
        .iter()
        .enumerate()
        .filter(|(_, r)| filter.accepts(r) && saved.as_ref().is_none_or(|s| searcher.matches(s, r)) && searcher.matches(&q, r))
        .map(|(i, _)| i)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    #[default]
    Key,
    Title,
    Author,
    Year,
    Status,
    Type,
}

impl SortField {
    pub const ALL: [SortField; 6] =
        [SortField::Key, SortField::Title, SortField::Author, SortField::Year, SortField::Status, SortField::Type];

    pub fn name(self) -> &'static str {
        match self {
            SortField::Key => "key",
            SortField::Title => "title",
            SortField::Author => "first author",
            SortField::Year => "year",
            SortField::Status => "reading status",
            SortField::Type => "type",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Sort {
    pub field: SortField,
    pub descending: bool,
}

/// Sorts `visible` (indices into `lib.refs`); ties keep key order, and references missing the
/// field go last either way.
pub fn sort(lib: &Library, visible: &mut [usize], by: Sort) {
    let status_rank = |r: &Ref| r.status.as_deref().and_then(|s| crate::library::STATUSES.iter().position(|x| *x == s));
    let text = |r: &Ref| -> Option<String> {
        match by.field {
            SortField::Key => Some(r.key.clone()),
            SortField::Title => Some(r.title.to_lowercase()).filter(|t| !t.is_empty()),
            SortField::Author => r.authors.first().map(|a| a.to_lowercase()),
            SortField::Type => Some(r.kind.clone()).filter(|t| !t.is_empty()),
            SortField::Year | SortField::Status => None,
        }
    };
    let number = |r: &Ref| match by.field {
        SortField::Year => r.year.map(i64::from),
        SortField::Status => status_rank(r).map(|p| p as i64),
        _ => None,
    };
    visible.sort_by(|&a, &b| {
        let (ra, rb) = (&lib.refs[a], &lib.refs[b]);
        let order = match by.field {
            SortField::Year | SortField::Status => cmp_present(number(ra), number(rb), by.descending),
            _ => cmp_present(text(ra), text(rb), by.descending),
        };
        order.then_with(|| ra.key.cmp(&rb.key))
    });
}

fn cmp_present<T: Ord>(a: Option<T>, b: Option<T>, descending: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Some(a), Some(b)) if descending => b.cmp(&a),
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
        (None, None) => Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_lists_parent_collections_and_filters_by_them() {
        let refs = [
            ("a".to_string(), "collections = [\"thesis/ch2\"]\ntags = [\"plain\", \"hot\"]\n".to_string()),
            ("b".into(), "title = \"Other\"\n".into()),
        ];
        let lib = Library::parse(Some("[tag_colors]\nhot = \"red\"\n[searches]\nmine = \"tag:plain\"\n"), None, refs, None).unwrap();
        let names: Vec<_> = sidebar(&lib).iter().map(|f| f.name().to_string()).collect();
        assert_eq!(names, ["All", "mine", "thesis", "thesis/ch2", "hot", "plain"]);
        let mut s = Searcher::new();
        assert_eq!(visible(&mut s, &lib, &Filter::Collection("thesis".into()), ""), [0]);
        assert_eq!(visible(&mut s, &lib, &Filter::Search("mine".into(), "tag:plain".into()), ""), [0]);
        assert_eq!(visible(&mut s, &lib, &Filter::All, "other"), [1]);
    }

    #[test]
    fn sorts_with_missing_values_last() {
        let refs = [
            ("a".to_string(), "year = 2020\nstatus = \"Read\"\n".to_string()),
            ("b".into(), "year = 2024\n".into()),
            ("c".into(), "status = \"New\"\n".into()),
        ];
        let lib = Library::parse(None, None, refs, None).unwrap();
        let sorted = |field, descending| {
            let mut v = vec![0, 1, 2];
            sort(&lib, &mut v, Sort { field, descending });
            v
        };
        assert_eq!(sorted(SortField::Year, false), [0, 1, 2]);
        assert_eq!(sorted(SortField::Year, true), [1, 0, 2]);
        assert_eq!(sorted(SortField::Status, false), [2, 0, 1]);
        assert_eq!(sorted(SortField::Key, true), [2, 1, 0]);
    }
}
