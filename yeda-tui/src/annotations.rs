use anyhow::{Context, Result, bail};
use lopdf::{Document, Object};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

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
    pub position: Option<toml::Table>,
    /// Fields written by other tools (e.g. yeda-reader), kept so rewriting the file never drops them.
    #[serde(flatten)]
    pub other: BTreeMap<String, toml::Value>,
}

#[derive(Default, Serialize, Deserialize)]
struct Sidecar {
    #[serde(default, rename = "annotation")]
    annotations: Vec<Annotation>,
}

pub fn load(dir: &Path) -> Result<Vec<Annotation>> {
    let path = dir.join("annotations.toml");
    if !path.exists() {
        return Ok(vec![]);
    }
    let s: Sidecar = toml::from_str(&fs::read_to_string(&path)?).with_context(|| path.display().to_string())?;
    Ok(s.annotations)
}

/// Written to a temporary file and renamed into place, since yeda-reader writes the same file.
pub fn save(dir: &Path, annotations: Vec<Annotation>) -> Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("annotations.toml.{}.tmp", std::process::id()));
    fs::write(&tmp, toml::to_string(&Sidecar { annotations })?)?;
    fs::rename(&tmp, dir.join("annotations.toml"))?;
    Ok(())
}

pub fn modified(dir: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(dir.join("annotations.toml")).ok()?.modified().ok()
}

/// Replaces the `source = "pdf"` annotations in `dir` with those embedded in `pdf`; returns their count.
pub fn sync_from_pdf(pdf: &Path, dir: &Path) -> Result<usize> {
    let mut kept = load(dir)?;
    for img in kept.iter().filter(|a| a.source == "pdf").filter_map(|a| a.image.as_ref()) {
        let _ = fs::remove_file(dir.join(img));
    }
    kept.retain(|a| a.source != "pdf");
    let extracted = extract(pdf, dir)?;
    let n = extracted.len();
    kept.extend(extracted);
    save(dir, kept)?;
    Ok(n)
}

fn extract(pdf: &Path, dir: &Path) -> Result<Vec<Annotation>> {
    let doc = Document::load(pdf).with_context(|| pdf.display().to_string())?;
    let pages = word_boxes(pdf)?;
    let mut out = Vec::new();
    for (num, page_id) in doc.get_pages() {
        let Some(page) = pages.get(num as usize - 1) else { continue };
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
            let id = format!("pdf-p{num}-{}", out.len());
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
                    quads.chunks_exact(8).map(|q| bbox(&[q[0], q[2], q[4], q[6]], &[q[1], q[3], q[5], q[7]])).collect()
                } else {
                    rect_box.into_iter().collect()
                }
            } else {
                rect_box.into_iter().collect()
            };
            if matches!(kind, "highlight" | "underline") {
                a.text = Some(words_in(page, &boxes)).filter(|s| !s.is_empty());
            } else if let ("image" | "ink", Some(b)) = (kind, rect_box) {
                let name = format!("img/{id}.png");
                render_region(pdf, num, page.height, b, &dir.join(&name))?;
                a.image = Some(name);
            }
            a.position = Some(position(num - 1, &boxes));
            a.id = id;
            out.push(a);
        }
    }
    Ok(out)
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
            String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>())
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

fn word_boxes(pdf: &Path) -> Result<Vec<Page>> {
    let out = Command::new("pdftotext")
        .arg("-bbox")
        .arg(pdf)
        .arg("-")
        .output()
        .context("running pdftotext (install poppler)")?;
    if !out.status.success() {
        bail!("pdftotext: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(parse_bbox(&String::from_utf8_lossy(&out.stdout)))
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
fn words_in(page: &Page, boxes: &[[f32; 4]]) -> String {
    page.words
        .iter()
        .filter(|w| {
            let (cx, cy) = ((w.x0 + w.x1) / 2.0, page.height - (w.y0 + w.y1) / 2.0);
            boxes.iter().any(|b| (b[0]..=b[2]).contains(&cx) && (b[1]..=b[3]).contains(&cy))
        })
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn pdf_text(pdf: &Path) -> Result<String> {
    let out = Command::new("pdftotext")
        .arg(pdf)
        .arg("-")
        .output()
        .context("running pdftotext (install poppler)")?;
    if !out.status.success() {
        bail!("pdftotext: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into())
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

/// Renders `rect` (PDF coordinates) of 1-based `page` to `png`.
pub fn render_area(pdf: &Path, page: u32, rect: [f32; 4], png: &Path) -> Result<()> {
    let p = page.to_string();
    let out = Command::new("pdfinfo")
        .args(["-f", &p, "-l", &p])
        .arg(pdf)
        .output()
        .context("running pdfinfo (install poppler)")?;
    let height = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("Page "))
        .find_map(|l| l.split_once(" size:")?.1.split_whitespace().nth(2)?.parse().ok())
        .with_context(|| format!("no size for page {page} of {}", pdf.display()))?;
    render_region(pdf, page, height, rect, png)
}

fn render_region(pdf: &Path, page: u32, height: f32, b: [f32; 4], png: &Path) -> Result<()> {
    const DPI: f32 = 150.0;
    let px = |v: f32| ((v * DPI / 72.0).round() as i64).to_string();
    fs::create_dir_all(png.parent().unwrap())?;
    let p = page.to_string();
    let out = Command::new("pdftoppm")
        .args(["-png", "-singlefile", "-r", &DPI.to_string(), "-f", &p, "-l", &p])
        .args(["-x", &px(b[0]), "-y", &px(height - b[3]), "-W", &px(b[2] - b[0]), "-H", &px(b[3] - b[1])])
        .arg(pdf)
        .arg(png.with_extension(""))
        .output()
        .context("running pdftoppm (install poppler)")?;
    if !out.status.success() {
        bail!("pdftoppm: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(words_in(&pages[0], &[quad]), "Hello &world");
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
}
