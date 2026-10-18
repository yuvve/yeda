//! Citations for export: BibTeX, or a bibliography formatted in a CSL style such as IEEE.

use crate::library::Ref;
use anyhow::{Context, Result, bail};
use hayagriva::archive::{ArchivedStyle, locales};
use hayagriva::citationberg::Style;
use hayagriva::{BibliographyDriver, BibliographyRequest, BufWriteFormat, CitationItem, CitationRequest};
use serde_json::{Map, Value, json};

/// What `formatted` renders with: a style hayagriva ships, by name, or a CSL file's contents.
pub enum CslStyle<'a> {
    Name(&'a str),
    Xml(&'a str),
}

/// `refs` in `format`: `bibtex`, a style name like `ieee`, or a path ending in `.csl`, which
/// `read_csl` reads.
pub fn export(refs: &[Ref], format: &str, read_csl: impl FnOnce(&str) -> Result<String>) -> Result<String> {
    match format {
        "bibtex" | "bib" => Ok(bibtex(refs)),
        "ident" => Ok(refs.iter().filter_map(identifier).map(|i| i + "\n").collect()),
        path if path.ends_with(".csl") => formatted(refs, CslStyle::Xml(&read_csl(path)?)),
        name => formatted(refs, CslStyle::Name(name)),
    }
}

/// The names of the styles hayagriva ships, such as `ieee` and `apa`.
pub fn style_names() -> Vec<&'static str> {
    ArchivedStyle::all().iter().map(|s| s.names()[0]).collect()
}

fn extra<'a>(r: &'a Ref, key: &str) -> Option<&'a str> {
    r.extra.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn arxiv_id(r: &Ref) -> Option<String> {
    extra(r, "eprint")
        .map(String::from)
        .or_else(|| extra(r, "archiveID").and_then(|a| a.strip_prefix("arXiv:")).map(String::from))
        .or_else(|| r.doi.as_deref().and_then(|d| d.to_lowercase().strip_prefix("10.48550/arxiv.").map(String::from)))
}

/// LaTeX's special characters in text fields; URLs and DOIs stay as they are.
fn latex(s: &str) -> String {
    s.chars().fold(String::new(), |mut out, c| {
        if "&%#_$".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

pub fn bibtex(refs: &[Ref]) -> String {
    refs.iter().map(bibtex_entry).collect::<Vec<_>>().join("\n")
}

fn bibtex_entry(r: &Ref) -> String {
    let kind = match r.kind.as_str() {
        "article" | "inproceedings" | "incollection" | "book" | "phdthesis" | "mastersthesis" | "techreport" => r.kind.as_str(),
        _ => "misc",
    };
    let arxiv = arxiv_id(r);
    let text = |k: &str| extra(r, k).map(latex);
    let fields: Vec<(&str, Option<String>)> = vec![
        ("author", Some(r.authors.iter().map(|a| latex(a)).collect::<Vec<_>>().join(" and ")).filter(|a| !a.is_empty())),
        ("title", Some(format!("{{{}}}", latex(&r.title)))),
        ("journal", text("journal")),
        ("booktitle", text("booktitle")),
        ("year", r.year.map(|y| y.to_string())),
        ("volume", text("volume")),
        ("number", text("number")),
        ("pages", extra(r, "pages").map(|p| p.replace('-', "--").replace("----", "--"))),
        ("publisher", text("publisher")),
        ("address", text("address")),
        ("isbn", text("isbn")),
        ("issn", text("issn")),
        ("doi", r.doi.clone()),
        ("url", extra(r, "url").map(String::from)),
        ("eprint", arxiv.clone()),
        ("archiveprefix", arxiv.map(|_| "arXiv".into())),
    ];
    let body: Vec<String> = fields.into_iter().filter_map(|(k, v)| Some(format!("  {k} = {{{}}}", v?))).collect();
    format!("@{kind}{{{},\n{}\n}}\n", r.key, body.join(",\n"))
}

/// hayagriva's YAML entry for `r`, as JSON (which is YAML too).
fn hayagriva_entry(r: &Ref) -> Value {
    let mut e = Map::new();
    let mut parent = Map::new();
    let (kind, parent_kind, parent_title) = match r.kind.as_str() {
        "article" => ("article", "periodical", extra(r, "journal")),
        "inproceedings" => ("article", "proceedings", extra(r, "booktitle")),
        "incollection" => ("chapter", "book", extra(r, "booktitle")),
        "book" => ("book", "", None),
        "preprint" => ("article", "", None),
        _ => ("misc", "", None),
    };
    e.insert("type".into(), kind.into());
    e.insert("title".into(), r.title.clone().into());
    e.insert("author".into(), r.authors.clone().into());
    let date = extra(r, "date").map(String::from).or_else(|| r.year.map(|y| y.to_string()));
    if let Some(d) = date {
        e.insert("date".into(), d.into());
    }
    if let Some(p) = extra(r, "pages") {
        e.insert("page-range".into(), p.into());
    }
    let mut serial = Map::new();
    if let Some(d) = &r.doi {
        serial.insert("doi".into(), d.clone().into());
    }
    if let Some(a) = arxiv_id(r) {
        // CSL's `number`, which styles print for preprints.
        if r.kind == "preprint" {
            serial.insert("serial".into(), format!("arXiv:{a}").into());
            e.insert("publisher".into(), json!({ "name": "arXiv" }));
        }
        serial.insert("arxiv".into(), a.into());
    }
    if let Some(i) = extra(r, "isbn") {
        serial.insert("isbn".into(), i.into());
    }
    if !serial.is_empty() {
        e.insert("serial-number".into(), Value::Object(serial));
    }
    if let Some(u) = extra(r, "url") {
        e.insert("url".into(), u.into());
    }
    let publisher = extra(r, "publisher").map(|p| json!({ "name": p, "location": extra(r, "address") }));
    // Volume and issue belong to the journal in hayagriva's model; a book has its own.
    let target = if parent_title.is_some() { &mut parent } else { &mut e };
    for (field, key) in [("volume", "volume"), ("issue", "number")] {
        if let Some(v) = extra(r, key) {
            target.insert(field.into(), v.into());
        }
    }
    if let Some(p) = publisher {
        target.insert("publisher".into(), p);
    }
    if let Some(t) = parent_title {
        parent.insert("type".into(), parent_kind.into());
        parent.insert("title".into(), t.into());
        e.insert("parent".into(), Value::Object(parent));
    }
    Value::Object(e)
}

/// One formatted bibliography entry per line, in the order given.
pub fn formatted(refs: &[Ref], style: CslStyle) -> Result<String> {
    let style = match style {
        CslStyle::Name(name) => match ArchivedStyle::by_name(name) {
            Some(s) => s.get(),
            None => bail!("no style named '{name}' (for example: ieee, apa, chicago-author-date, vancouver; or a .csl file)"),
        },
        CslStyle::Xml(xml) => Style::from_xml(xml).context("reading the CSL style")?,
    };
    let Style::Independent(style) = style else {
        bail!("this is a dependent CSL style; use the independent style it points to")
    };
    let entries: Map<String, Value> = refs.iter().map(|r| (r.key.clone(), hayagriva_entry(r))).collect();
    let library = hayagriva::io::from_yaml_str(&Value::Object(entries).to_string()).context("building the bibliography")?;
    let locales = locales();
    let mut driver = BibliographyDriver::new();
    // Cited in the given order, so numeric styles like IEEE number the entries 1, 2, 3, …
    for entry in library.iter() {
        driver.citation(CitationRequest::from_items(vec![CitationItem::with_entry(entry)], &style, &locales));
    }
    let rendered = driver.finish(BibliographyRequest { style: &style, locale: None, locale_files: &locales });
    let bibliography = rendered.bibliography.context("this style has no bibliography")?;
    let mut out = String::new();
    for item in bibliography.items {
        if let Some(first) = item.first_field {
            first.write_buf(&mut out, BufWriteFormat::Plain)?;
            out.push(' ');
        }
        item.content.write_buf(&mut out, BufWriteFormat::Plain)?;
        out.push('\n');
    }
    Ok(out)
}

/// The reference's identifier in a form `a` (add) takes: its DOI, else arXiv ID, else first ISBN, else URL.
pub fn identifier(r: &Ref) -> Option<String> {
    let extra = |name: &str| r.extra.get(name).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty());
    r.doi
        .clone()
        .or_else(|| extra("archiveID").filter(|a| a.starts_with("arXiv:")).map(String::from))
        .or_else(|| extra("isbn").and_then(|i| i.split_whitespace().next()).map(String::from))
        .or_else(|| extra("url").map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_identifiers() {
        let with = |field: &str, value: &str| {
            let mut r = Ref::default();
            r.extra.insert(field.into(), value.into());
            r
        };
        let mut arxiv = with("archiveID", "arXiv:1901.08492");
        arxiv.extra.insert("url".into(), "https://arxiv.org/abs/1901.08492".into());
        let refs = [paper(), arxiv, with("isbn", "978-0-262-38051-5 978-0-262-38050-8"), with("url", "https://example.org/x"), Ref::default()];
        let text = export(&refs, "ident", |_| unreachable!()).unwrap();
        assert_eq!(text, "10.3390/s22239042\narXiv:1901.08492\n978-0-262-38051-5\nhttps://example.org/x\n");
    }

    fn paper() -> Ref {
        let mut r = Ref {
            key: "abu-aisheh2022".into(),
            kind: "article".into(),
            title: "CARA: Connectivity-Aware Relay Algorithm for Multi-Robot Expeditions".into(),
            authors: vec!["Abu-Aisheh, Razanne".into(), "Bronzino, Francesco".into()],
            year: Some(2022),
            doi: Some("10.3390/s22239042".into()),
            ..Default::default()
        };
        for (k, v) in [("journal", "Sensors"), ("volume", "22"), ("number", "23"), ("pages", "9042"), ("date", "2022-11-22")] {
            r.extra.insert(k.into(), v.into());
        }
        r
    }

    #[test]
    fn writes_bibtex() {
        let b = bibtex(&[paper()]);
        assert!(b.starts_with("@article{abu-aisheh2022,\n"), "{b}");
        assert!(b.contains("  author = {Abu-Aisheh, Razanne and Bronzino, Francesco},\n"));
        assert!(b.contains("  journal = {Sensors},\n") && b.contains("  doi = {10.3390/s22239042}"));
        let mut pre = Ref { key: "p".into(), kind: "preprint".into(), title: "A & B".into(), ..Default::default() };
        pre.extra.insert("archiveID".into(), "arXiv:1901.08492".into());
        let b = bibtex(&[pre]);
        assert!(b.starts_with("@misc{p,") && b.contains("eprint = {1901.08492}") && b.contains("title = {{A \\& B}}"), "{b}");
    }

    #[test]
    fn formats_preprints() {
        let mut pre = Ref { key: "p".into(), kind: "preprint".into(), title: "Feudal".into(), authors: vec!["Ahilan, S.".into()], year: Some(2019), ..Default::default() };
        pre.extra.insert("archiveID".into(), "arXiv:1901.08492".into());
        let ieee = formatted(std::slice::from_ref(&pre), CslStyle::Name("ieee")).unwrap();
        assert_eq!(ieee, "[1] S. Ahilan, “Feudal,” 2019, arXiv: arXiv:1901.08492.\n");
    }

    #[test]
    fn formats_ieee() {
        let out = formatted(&[paper()], CslStyle::Name("ieee")).unwrap();
        assert!(out.starts_with("[1] R. Abu-Aisheh and F. Bronzino"), "{out}");
        assert!(out.contains("Sensors") && out.contains("vol. 22") && out.contains("2022"), "{out}");
        assert!(formatted(&[paper()], CslStyle::Name("nope")).is_err());
    }
}
