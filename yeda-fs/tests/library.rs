//! Checks that `testdata/library` still loads: every reference, note, table row and annotation.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use yeda_core::annotations;
use yeda_core::browse::{self, Filter};
use yeda_core::library::STATUSES;
use yeda_core::search::Searcher;

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../testdata")
}

fn load() -> yeda_core::library::Library {
    let dir = testdata().join("library");
    let lib = yeda_fs::load_library(&dir, None, None).unwrap();
    assert!(lib.unreadable.is_empty(), "left out: {:?}", lib.unreadable);
    lib
}

#[test]
fn references_carry_their_metadata() {
    let lib = load();
    assert_eq!(lib.refs.len(), 27);
    let keys: BTreeSet<&str> = lib.refs.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys.len(), lib.refs.len(), "two references share a citekey");
    let dois: BTreeSet<&str> = lib.refs.iter().filter_map(|r| r.doi.as_deref()).collect();
    assert_eq!(dois.len(), lib.refs.len(), "a reference has no DOI, or two share one");
    for r in &lib.refs {
        assert!(!r.title.is_empty() && !r.authors.is_empty() && r.year.is_some(), "{}", r.key);
        assert!(STATUSES.contains(&r.status.as_str()), "{}: unknown status {:?}", r.key, r.status);
        assert!(!r.tags.is_empty() && !r.collections.is_empty(), "{}: no tags or collections", r.key);
    }
}

#[test]
fn notes_and_table_rows_belong_to_references() {
    let lib = load();
    assert!(!lib.tables.is_empty());
    for entry in fs::read_dir(testdata().join("library/notes")).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let key = name.strip_suffix(".ai.toml").or_else(|| name.strip_suffix(".md")).unwrap();
        assert!(lib.get(key).is_some(), "notes/{name} has no reference");
    }
    let rows = lib.refs.iter().filter(|r| !r.tables.is_empty()).count();
    assert!(rows >= 8, "only {rows} references have table rows");
    for r in &lib.refs {
        for (name, row) in &r.tables {
            let table = lib.tables.get(name).unwrap_or_else(|| panic!("{}: no table {name}", r.key));
            for (column, value) in row {
                assert!(table.columns.contains(column), "{}: table {name} has no column {column}", r.key);
                assert!(!value.trim().is_empty(), "{}: {name}.{column} is empty", r.key);
            }
        }
    }
}

#[test]
fn annotations_parse_and_their_images_exist() {
    let lib = load();
    let root = testdata().join("library/annotations");
    for entry in fs::read_dir(&root).unwrap() {
        let dir = entry.unwrap().path();
        let key = dir.file_name().unwrap().to_str().unwrap();
        let r = lib.get(key).unwrap_or_else(|| panic!("annotations/{key} has no reference"));
        for pdf_dir in fs::read_dir(&dir).unwrap() {
            let pdf_dir = pdf_dir.unwrap().path();
            let id = pdf_dir.file_name().unwrap().to_str().unwrap();
            assert!(r.pdfs.iter().any(|p| p.id == id), "{key}: no PDF {id}");
            let text = fs::read_to_string(pdf_dir.join(annotations::FILE)).unwrap();
            let list = annotations::parse(&text).unwrap();
            assert!(!list.is_empty(), "{key}/{id}");
            for a in &list {
                assert!(a.id.chars().all(|c| c.is_ascii_alphanumeric()), "{key}: id {:?}", a.id);
                assert!(a.sort_index.is_some(), "{key}: {} has no sort_index", a.id);
                let position = a.position.as_ref().expect("no position");
                assert!(position.contains_key("pageIndex") && position.contains_key("rects"), "{key}: {}", a.id);
                if let Some(image) = &a.image {
                    assert!(pdf_dir.join(image).exists(), "{key}: missing {image}");
                }
            }
        }
    }
}

#[test]
fn saved_searches_find_papers() {
    let lib = load();
    assert!(lib.searches.len() >= 6);
    let mut searcher = Searcher::new();
    for (name, saved) in &lib.searches {
        let filter = Filter::Search(name.clone(), saved.query.clone());
        let found = browse::visible(&mut searcher, &lib, &filter, "");
        assert!(!found.is_empty(), "saved search {name:?} finds nothing");
    }
}

#[test]
fn open_access_papers_have_a_pdf() {
    let lib = load();
    let list = fs::read_to_string(testdata().join("pdfs.tsv")).unwrap();
    let listed: Vec<(&str, &str)> = list
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.split_once('\t').unwrap_or_else(|| panic!("pdfs.tsv: no tab in {l:?}")))
        .collect();
    assert!(!listed.is_empty());
    for (key, url) in listed {
        assert!(url.starts_with("https://"), "{key}: {url}");
        let r = lib.get(key).unwrap_or_else(|| panic!("pdfs.tsv names {key}, which is not in the library"));
        let path = format!("{key}/{key}.pdf");
        assert!(r.pdfs.iter().any(|p| p.location == "papers" && p.path == path), "{key}: no [[pdf]] for {path}");
    }
}
