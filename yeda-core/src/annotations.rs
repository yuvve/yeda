use crate::Effect;
use anyhow::{Context, Result, bail};
use lopdf::{Document, Object};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::collections::BTreeMap;
use toml::{Table, Value};

pub const FILE: &str = "annotations.toml";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Annotation {
    pub id: String,
    pub kind: String,
    pub page: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_index: Option<String>,
    /// Zotero's position JSON with its keys verbatim (`pageIndex`, `rects`, `paths`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Table>,
    /// Fields written by other clients, kept so rewriting the file never drops them.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

#[derive(Default, Serialize, Deserialize)]
struct Sidecar {
    #[serde(default, rename = "annotation")]
    annotations: Vec<Annotation>,
}

/// `annotations/<key>/annotations.toml`.
pub fn file_path(key: &str) -> String {
    format!("{}/{FILE}", crate::library::annotations_dir(key))
}

pub fn parse(text: &str) -> Result<Vec<Annotation>> {
    Ok(toml::from_str::<Sidecar>(text).context(FILE)?.annotations)
}

pub fn render(annotations: Vec<Annotation>) -> Result<String> {
    Ok(toml::to_string(&Sidecar { annotations })?)
}

/// `img/<id>.png`, relative to the annotations folder; rejects ids that could escape it.
pub fn image_name(id: &str) -> Result<String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("invalid annotation id: {id:?}");
    }
    Ok(format!("img/{id}.png"))
}

/// The entries as JSON objects with their keys verbatim, as zotero/reader takes them.
pub fn to_json(text: Option<&str>) -> Result<Vec<Json>> {
    let mut doc = read(text)?;
    entries(&mut doc)?.drain(..).map(|e| Ok(serde_json::to_value(e)?)).collect()
}

/// Merges `changes` by `id`: a key set to null is removed, other keys are replaced, keys absent
/// from a change are kept, and unknown ids are appended. `text` must be re-read just before.
pub fn merge(text: Option<&str>, changes: Vec<Map<String, Json>>) -> Result<String> {
    let mut doc = read(text)?;
    let entries = entries(&mut doc)?;
    for change in changes {
        let id = change.get("id").and_then(Json::as_str).context("annotation without id")?.to_owned();
        let idx = entries.iter().position(|e| e.get("id").and_then(Value::as_str) == Some(&id));
        let idx = idx.unwrap_or_else(|| {
            entries.push(Value::Table(Table::new()));
            entries.len() - 1
        });
        let entry = entries[idx].as_table_mut().with_context(|| format!("annotation {id} is not a table"))?;
        for (key, value) in change {
            match json_to_toml(value) {
                Some(v) => entry.insert(key, v),
                None => entry.remove(&key),
            };
        }
    }
    Ok(toml::to_string(&doc)?)
}

/// Drops the entries with these ids; returns the new file and the image files to remove.
pub fn delete(text: Option<&str>, ids: &[String]) -> Result<(String, Vec<String>)> {
    let mut doc = read(text)?;
    entries(&mut doc)?.retain(|e| !e.get("id").and_then(Value::as_str).is_some_and(|id| ids.iter().any(|d| d == id)));
    let images = ids.iter().map(|id| image_name(id)).collect::<Result<_>>()?;
    Ok((toml::to_string(&doc)?, images))
}

/// Merges two edits of one file by `id`. An entry changed on one side takes that side; changed
/// on both, `ours` wins. Deleting wins over leaving unchanged, and loses to a change.
pub fn merge3(base: Option<&str>, ours: &str, theirs: &str) -> Result<String> {
    let mut doc = read(Some(ours))?;
    let by_id = |doc: &mut Table| -> Result<Vec<(String, Value)>> {
        let id = |e: &Value| e.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        Ok(entries(doc)?.drain(..).map(|e| (id(&e), e)).collect())
    };
    let (base, ours, theirs) = (by_id(&mut read(base)?)?, by_id(&mut doc)?, by_id(&mut read(Some(theirs))?)?);
    let find = |list: &[(String, Value)], id: &str| list.iter().find(|(i, _)| i == id).map(|(_, e)| e.clone());
    // ponytail: linear lookups, fine for the few hundred annotations one paper has.
    let mut merged = Vec::new();
    for (id, mine) in &ours {
        let old = find(&base, id);
        match find(&theirs, id) {
            Some(other) if old.as_ref() == Some(mine) => merged.push(other),
            None if old.as_ref() == Some(mine) => {}
            _ => merged.push(mine.clone()),
        }
    }
    for (id, other) in theirs {
        if find(&ours, &id).is_none() && find(&base, &id).is_none_or(|old| old != other) {
            merged.push(other);
        }
    }
    *entries(&mut doc)? = merged;
    Ok(toml::to_string(&doc)?)
}

fn read(text: Option<&str>) -> Result<Table> {
    Ok(text.map(str::parse).transpose().context(FILE)?.unwrap_or_default())
}

fn entries(doc: &mut Table) -> Result<&mut Vec<Value>> {
    doc.entry("annotation")
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
        .context("`annotation` is not an array")
}

/// TOML has no null, so nulls become `None`; inside arrays and tables they are dropped.
fn json_to_toml(v: Json) -> Option<Value> {
    Some(match v {
        Json::Null => return None,
        Json::Bool(b) => Value::Boolean(b),
        Json::Number(n) => n.as_i64().map(Value::Integer).unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => Value::String(s),
        Json::Array(a) => Value::Array(a.into_iter().filter_map(json_to_toml).collect()),
        Json::Object(o) => Value::Table(o.into_iter().filter_map(|(k, v)| Some((k, json_to_toml(v)?))).collect()),
    })
}

/// Renders `rect` (PDF coordinates) of 1-based `page`, whose height is `height`, to PNG bytes.
pub type RenderPng<'a> = dyn FnMut(u32, f32, [f32; 4]) -> Option<Vec<u8>> + 'a;

/// Replaces the `source = "pdf"` annotations in `dir` (library-relative) with those embedded in
/// `pdf`. `word_boxes` is `pdftotext -bbox` output, used to recover highlighted text; without it
/// highlights carry no text. Returns the count and the changes to apply.
pub fn sync_from_pdf(dir: &str, existing: Option<&str>, pdf: &[u8], word_boxes: Option<&str>, render_png: &mut RenderPng) -> Result<(usize, Vec<Effect>)> {
    let mut kept = existing.map(parse).transpose()?.unwrap_or_default();
    let mut effects: Vec<Effect> = kept
        .iter()
        .filter(|a| a.source == "pdf")
        .filter_map(|a| a.image.as_ref())
        .map(|img| Effect::remove(format!("{dir}/{img}")))
        .collect();
    kept.retain(|a| a.source != "pdf");
    let pages = word_boxes.map(parse_bbox).unwrap_or_default();
    let (extracted, images) = extract(dir, pdf, &pages, render_png)?;
    let n = extracted.len();
    kept.extend(extracted);
    effects.extend(images.into_iter().map(|(name, png)| Effect::Write { path: format!("{dir}/{name}"), bytes: png }));
    effects.push(Effect::write(format!("{dir}/{FILE}"), render(kept)?));
    Ok((n, effects))
}

/// Image file names (relative to the annotations folder) with their PNG bytes.
type Images = Vec<(String, Vec<u8>)>;

fn extract(dir: &str, pdf: &[u8], pages: &[Page], render_png: &mut RenderPng) -> Result<(Vec<Annotation>, Images)> {
    let doc = Document::load_mem(pdf).context("reading pdf")?;
    let (mut out, mut images) = (Vec::new(), Vec::new());
    for (num, page_id) in doc.get_pages() {
        let no_words = Page { height: media_height(&doc, page_id), words: vec![] };
        let page = pages.get(num as usize - 1).unwrap_or(&no_words);
        for annot in doc.get_page_annotations(page_id).unwrap_or_default() {
            let kind = match annot.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"") {
                b"Highlight" => "highlight",
                b"Underline" | b"StrikeOut" | b"Squiggly" => "underline",
                b"Text" => "note",
                b"FreeText" => "text",
                b"Square" => "image",
                b"Ink" => "ink",
                _ => continue,
            };
            let get = |key: &[u8]| annot.get(key).ok().and_then(|o| doc.dereference(o).ok()).map(|(_, o)| o);
            let rect = numbers(&doc, get(b"Rect"));
            let rect_box = (rect.len() == 4).then(|| bbox(&[rect[0], rect[2]], &[rect[1], rect[3]]));
            let mut a = Annotation {
                kind: kind.into(),
                page: num.to_string(),
                color: color(&numbers(&doc, get(b"C"))),
                comment: get(b"Contents").and_then(text_string).filter(|s| !s.trim().is_empty()),
                source: "pdf".into(),
                ..Default::default()
            };
            let boxes: Vec<_> = if matches!(kind, "highlight" | "underline") {
                let quads = numbers(&doc, get(b"QuadPoints"));
                if quads.len() >= 8 {
                    quads.as_chunks::<8>().0.iter().map(|q| bbox(&[q[0], q[2], q[4], q[6]], &[q[1], q[3], q[5], q[7]])).collect()
                } else {
                    rect_box.into_iter().collect()
                }
            } else {
                rect_box.into_iter().collect()
            };
            let id = crate::ids::zotero_key(&format!("{dir}|{kind}|{num}|{boxes:?}"));
            let mut first_word = 0;
            if matches!(kind, "highlight" | "underline") {
                let (text, first) = words_in(page, &boxes);
                a.text = Some(text).filter(|s| !s.is_empty());
                first_word = first;
            } else if let ("image" | "ink", Some(b)) = (kind, rect_box)
                && let Some(png) = render_png(num, page.height, b)
            {
                let name = image_name(&id)?;
                images.push((name.clone(), png));
                a.image = Some(name);
            }
            let top = boxes.iter().map(|b| b[3]).fold(f32::NEG_INFINITY, f32::max);
            a.sort_index = Some(sort_index(num - 1, first_word, (page.height - top).max(0.0)));
            a.position = Some(position(num - 1, &boxes));
            a.id = id;
            out.push(a);
        }
    }
    Ok((out, images))
}

/// Zotero's `annotationSortIndex`: page index, character offset on the page, distance from the page top.
fn sort_index(page_index: u32, offset: usize, top: f32) -> String {
    format!("{page_index:05}|{offset:06}|{:05}", top as u32)
}

fn media_height(doc: &Document, page: lopdf::ObjectId) -> f32 {
    let media = doc.get_dictionary(page).ok().and_then(|d| d.get(b"MediaBox").ok());
    match numbers(doc, media)[..] {
        [_, y0, _, y1] => y1 - y0,
        _ => 792.0,
    }
}

fn position(page_index: u32, rects: &[[f32; 4]]) -> toml::Table {
    let round = |v: f32| toml::Value::Float((v as f64 * 1000.0).round() / 1000.0);
    let rects = rects.iter().map(|r| toml::Value::Array(r.iter().copied().map(round).collect())).collect();
    toml::Table::from_iter([
        ("pageIndex".to_string(), toml::Value::Integer(page_index.into())),
        ("rects".to_string(), toml::Value::Array(rects)),
    ])
}

fn numbers(doc: &Document, obj: Option<&Object>) -> Vec<f32> {
    obj.and_then(|o| o.as_array().ok())
        .into_iter()
        .flatten()
        .filter_map(|o| doc.dereference(o).ok()?.1.as_float().ok())
        .collect()
}

/// `[x0, y0, x1, y1]` in PDF coordinates (origin bottom-left).
fn bbox(xs: &[f32], ys: &[f32]) -> [f32; 4] {
    let min = |v: &[f32]| v.iter().copied().fold(f32::INFINITY, f32::min);
    let max = |v: &[f32]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    [min(xs), min(ys), max(xs), max(ys)]
}

fn color(c: &[f32]) -> Option<String> {
    let [r, g, b] = c else { return None };
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(format!("#{:02x}{:02x}{:02x}", byte(*r), byte(*g), byte(*b)))
}

fn text_string(obj: &Object) -> Option<String> {
    let bytes = obj.as_str().ok()?;
    Some(match bytes {
        [0xFE, 0xFF, rest @ ..] => {
            String::from_utf16_lossy(&rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect::<Vec<_>>())
        }
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into(),
        // ponytail: PDFDocEncoding read as Latin-1; differs only in a few punctuation code points.
        _ => bytes.iter().map(|&b| b as char).collect(),
    })
}

struct Word {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    text: String,
}

struct Page {
    height: f32,
    words: Vec<Word>,
}

fn attr(line: &str, name: &str) -> f32 {
    line.split_once(&format!(" {name}=\""))
        .and_then(|(_, rest)| rest.split('"').next()?.parse().ok())
        .unwrap_or(0.0)
}

fn parse_bbox(html: &str) -> Vec<Page> {
    let mut pages = Vec::new();
    for line in html.lines().map(str::trim) {
        if line.starts_with("<page ") {
            pages.push(Page { height: attr(line, "height"), words: vec![] });
        } else if let (true, Some(page)) = (line.starts_with("<word "), pages.last_mut()) {
            let text = line.split_once('>').and_then(|(_, r)| r.strip_suffix("</word>")).unwrap_or("");
            page.words.push(Word {
                x0: attr(line, "xMin"),
                y0: attr(line, "yMin"),
                x1: attr(line, "xMax"),
                y1: attr(line, "yMax"),
                text: unescape_html(text),
            });
        }
    }
    pages
}

// ponytail: assumes an unrotated page whose CropBox starts at the origin; rotated or offset pages pick the wrong words.
/// The words under `boxes`, and the index on the page of the first one (0 if none).
fn words_in(page: &Page, boxes: &[[f32; 4]]) -> (String, usize) {
    let hits: Vec<(usize, &Word)> = page
        .words
        .iter()
        .enumerate()
        .filter(|(_, w)| {
            let (cx, cy) = ((w.x0 + w.x1) / 2.0, page.height - (w.y0 + w.y1) / 2.0);
            boxes.iter().any(|b| (b[0]..=b[2]).contains(&cx) && (b[1]..=b[3]).contains(&cy))
        })
        .collect();
    let text = hits.iter().map(|(_, w)| w.text.as_str()).collect::<Vec<_>>().join(" ");
    (text, hits.first().map_or(0, |(i, _)| *i))
}

pub fn unescape_html(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge3_combines_both_sides_by_id() {
        let entry = |id: &str, comment: &str| format!("[[annotation]]\nid = \"{id}\"\ncomment = \"{comment}\"\n");
        let base = [entry("A", "a"), entry("B", "b"), entry("C", "c"), entry("D", "d")].concat();
        // Ours edits A, deletes C, adds E; theirs edits B, deletes D, adds F.
        let ours = [entry("A", "a2"), entry("B", "b"), entry("D", "d"), entry("E", "e")].concat();
        let theirs = [entry("A", "a"), entry("B", "b2"), entry("C", "c"), entry("F", "f")].concat();
        let merged = parse(&merge3(Some(&base), &ours, &theirs).unwrap()).unwrap();
        let got: Vec<_> = merged.iter().map(|a| format!("{}={}", a.id, a.comment.as_deref().unwrap())).collect();
        assert_eq!(got, ["A=a2", "B=b2", "E=e", "F=f"]);
    }

    #[test]
    fn selects_words_under_quads() {
        let pages = parse_bbox(
            r#"<page width="612.000000" height="792.000000">
    <word xMin="72.0" yMin="70.0" xMax="100.0" yMax="82.0">Hello</word>
    <word xMin="104.0" yMin="70.0" xMax="140.0" yMax="82.0">&amp;world</word>
    <word xMin="72.0" yMin="90.0" xMax="100.0" yMax="102.0">below</word>
</page>"#,
        );
        assert_eq!(pages.len(), 1);
        // Line one spans y 70..82 from the top, i.e. 710..722 in PDF coordinates.
        let quad = bbox(&[70.0, 145.0], &[708.0, 724.0]);
        assert_eq!(words_in(&pages[0], &[quad]), ("Hello &world".to_string(), 0));
    }

    #[test]
    fn keeps_unknown_fields_and_position() {
        let src = r#"
[[annotation]]
id = "ABCD1234"
kind = "ink"
page = "5"
source = "reader"
pen_pressure = [0.4, 0.6]
tags = [{ name = "x", type = 0 }]
dateModified = "2026-10-14T12:00:00Z"

[annotation.reader_state]
zoom = 1.5

[annotation.position]
pageIndex = 4
paths = [[1.5, 2.0, 3.0, 4.0]]
width = 2.0
"#;
        let s: Sidecar = toml::from_str(src).unwrap();
        let a = &s.annotations[0];
        assert_eq!(a.position.as_ref().unwrap()["pageIndex"].as_integer(), Some(4));
        assert!(a.other.contains_key("pen_pressure"));
        assert!(a.other["reader_state"].is_table());
        let again: Sidecar = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(again.annotations, s.annotations);
        assert_eq!(position(0, &[[1.0, 2.25, 3.0, 4.0]])["rects"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn decodes_strings_and_colors() {
        let utf16 = Object::string_literal([0xFE, 0xFF, 0x00, 0x48, 0x00, 0xE9].to_vec());
        assert_eq!(text_string(&utf16).as_deref(), Some("Hé"));
        assert_eq!(color(&[1.0, 0.82, 0.0]).as_deref(), Some("#ffd100"));
        assert_eq!(color(&[0.5]), None);
    }

    #[test]
    fn merges_by_id_and_round_trips() {
        use serde_json::json;
        let file = r#"
[[annotation]]
id = "KEEP0001"
kind = "note"
page = "1"
source = "zotero"
pen_pressure = [0.4, 0.6]

[[annotation]]
id = "EDIT0001"
kind = "highlight"
page = "5"
text = "old text"
comment = "old comment"
source = "reader"

[annotation.position]
pageIndex = 4
rects = [[49.5, 41.1, 568.2, 210.3]]
"#;
        let change = |v: Json| v.as_object().unwrap().clone();
        let merged = merge(
            Some(file),
            vec![
                change(json!({ "id": "EDIT0001", "comment": "new comment", "text": null })),
                change(json!({ "id": "NEW00001", "kind": "ink", "page": "2", "source": "reader",
                    "position": { "pageIndex": 1, "paths": [[1.5, 2.0, 3.0, 4.0]], "width": 2.0 } })),
            ],
        )
        .unwrap();

        let loaded = to_json(Some(&merged)).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0]["pen_pressure"], json!([0.4, 0.6]));
        assert_eq!(loaded[1]["comment"], "new comment");
        assert!(loaded[1].get("text").is_none());
        assert_eq!(loaded[1]["position"]["rects"], json!([[49.5, 41.1, 568.2, 210.3]]));
        assert_eq!(loaded[2]["position"]["paths"], json!([[1.5, 2.0, 3.0, 4.0]]));

        let again = merge(Some(&merged), loaded.into_iter().map(|v| v.as_object().unwrap().clone()).collect()).unwrap();
        assert_eq!(again, merged);
        assert_eq!(parse(&merged).unwrap()[1].comment.as_deref(), Some("new comment"));

        let (after, images) = delete(Some(&merged), &["NEW00001".into()]).unwrap();
        assert_eq!(to_json(Some(&after)).unwrap().len(), 2);
        assert_eq!(images, vec!["img/NEW00001.png"]);
        assert!(image_name("../escape").is_err());
        assert!(to_json(None).unwrap().is_empty());
    }

    #[test]
    fn syncs_annotations_embedded_in_pdf() {
        use lopdf::{Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let annot = |subtype: &str, rect: [f32; 4]| {
            dictionary! { "Type" => "Annot", "Subtype" => subtype, "Rect" => rect.iter().map(|&v| v.into()).collect::<Vec<Object>>(), "C" => vec![1.into(), 0.into(), 0.into()] }
        };
        let highlight = doc.add_object(annot("Highlight", [72.0, 708.0, 145.0, 724.0]));
        let square = doc.add_object(annot("Square", [10.0, 10.0, 50.0, 50.0]));
        let content = doc.add_object(Stream::new(dictionary! {}, vec![]));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()], "Annots" => vec![highlight.into(), square.into()],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut pdf = Vec::new();
        doc.save_to(&mut pdf).unwrap();

        let existing = "[[annotation]]\nid = \"old\"\nkind = \"image\"\npage = \"1\"\nsource = \"pdf\"\nimage = \"img/old.png\"\n\n[[annotation]]\nid = \"mine\"\nkind = \"note\"\npage = \"1\"\nsource = \"reader\"\n";
        let words = "<page width=\"612\" height=\"792\">\n<word xMin=\"72.0\" yMin=\"70.0\" xMax=\"100.0\" yMax=\"82.0\">Hello</word>\n</page>";
        let mut heights = vec![];
        let mut render = |_page, height, _rect| {
            heights.push(height);
            Some(b"png".to_vec())
        };
        let (n, effects) = sync_from_pdf("annotations/k", Some(existing), &pdf, Some(words), &mut render).unwrap();
        assert_eq!(n, 2);
        assert_eq!(heights, [792.0]);
        assert_eq!(effects[0], Effect::remove("annotations/k/img/old.png"));
        let Effect::Write { path, bytes } = &effects[2] else { panic!() };
        assert_eq!(path, "annotations/k/annotations.toml");
        let all = parse(std::str::from_utf8(bytes).unwrap()).unwrap();
        let fields: Vec<_> = all.iter().map(|a| (a.text.as_deref(), a.color.as_deref(), a.sort_index.as_deref())).collect();
        assert_eq!(fields, [
            (None, None, None),
            (Some("Hello"), Some("#ff0000"), Some("00000|000000|00068")),
            (None, Some("#ff0000"), Some("00000|000000|00742")),
        ]);
        let zotero_key = |id: &str| id.len() == 8 && id.bytes().all(|b| b"23456789ABCDEFGHIJKLMNPQRSTUVWXYZ".contains(&b));
        assert!(all[1..].iter().all(|a| zotero_key(&a.id)), "{all:?}");
        assert_ne!(all[1].id, all[2].id);
        assert_eq!(effects[1], Effect::write(format!("annotations/k/img/{}.png", all[2].id), "png"));
        let (_, again) = sync_from_pdf("annotations/k", Some(existing), &pdf, Some(words), &mut |_, _, _| None).unwrap();
        let Effect::Write { bytes: again, .. } = again.last().unwrap() else { panic!() };
        let again = parse(std::str::from_utf8(again).unwrap()).unwrap();
        assert_eq!(again.iter().map(|a| &a.id).collect::<Vec<_>>(), all.iter().map(|a| &a.id).collect::<Vec<_>>());
    }
}
