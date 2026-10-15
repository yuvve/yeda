use crate::library::{Library, Ref};
use crate::search::{self, Searcher};
use std::collections::BTreeSet;

/// An entry in the library sidebar.
#[derive(Debug, Clone, PartialEq)]
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
}
