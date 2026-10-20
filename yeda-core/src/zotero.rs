use crate::Effect;
use crate::annotations::{self, Annotation};
use crate::library::{self, Library, Pdf, Ref, config_section};
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

const NOT_DELETED: &str = "NOT IN (SELECT itemID FROM deletedItems)";

#[derive(Default)]
struct Summary {
    refs: usize,
    skipped: usize,
    annotations: usize,
    notes: usize,
    images: usize,
    images_missing: usize,
    pdfs_unresolved: usize,
    searches: usize,
    searches_skipped: Vec<String>,
    tag_colors: usize,
}

/// Where an image or ink annotation's PNG can come from: Zotero's render cache, else the region
/// `rect` (PDF coordinates) of 1-based `page` of `pdf`.
pub struct ImageSource<'a> {
    pub cached: PathBuf,
    pub pdf: Option<&'a Path>,
    pub area: Option<(u32, [f32; 4])>,
}

pub type LoadImage<'a> = dyn FnMut(&ImageSource) -> Option<Vec<u8>> + 'a;

/// Imports every item of Zotero's database `db` not yet in `lib`. `zotero` is the Zotero data
/// folder, used to build attachment paths. Returns a report and the changes to apply.
/// `linked` is Zotero's base folder for relative linked files (its `baseAttachmentPath` setting).
pub fn import(db: &Connection, zotero: &Path, linked: Option<&Path>, mut lib: Library, home: Option<&Path>, image: &mut LoadImage) -> Result<(String, Vec<Effect>)> {
    let mut effects = Vec::new();
    let mut new_locations = Vec::new();
    ensure_location(&mut lib, &mut new_locations, "zotero", &zotero.join("storage"));

    let imported: HashSet<String> = lib
        .refs
        .iter()
        .filter_map(|r| r.extra.get("zotero_key")?.as_str().map(String::from))
        .collect();
    let (collections, collections_by_key) = collection_paths(db)?;
    let mut sum = Summary::default();

    let mut items = db.prepare(&format!(
        "SELECT i.itemID, i.key, t.typeName FROM items i JOIN itemTypes t USING(itemTypeID)
         WHERE t.typeName NOT IN ('attachment', 'annotation', 'note') AND i.itemID {NOT_DELETED}"
    ))?;
    let items: Vec<(i64, String, String)> =
        items.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;

    for (id, zkey, typ) in items {
        if imported.contains(&zkey) {
            sum.skipped += 1;
            continue;
        }
        let mut r = Ref { kind: bib_type(&typ).into(), ..Default::default() };
        r.extra.insert("zotero_key".into(), zkey.into());
        let mut citation_key = None;
        for (field, value) in query_pairs(db, "SELECT f.fieldName, CAST(v.value AS TEXT) FROM itemData d JOIN fieldsCombined f USING(fieldID) JOIN itemDataValues v USING(valueID) WHERE d.itemID = ?1", id)? {
            match field.as_str() {
                "title" => r.title = value,
                "DOI" => r.doi = Some(value.to_lowercase()),
                "citationKey" => citation_key = Some(value),
                "extra" => {
                    let mut note = Vec::new();
                    for line in value.lines() {
                        if let Some(status) = line.strip_prefix("Read_Status:") {
                            r.status = Some(status.trim().into());
                        } else if let Some(date) = line.strip_prefix("Read_Status_Date:") {
                            r.extra.insert("status_date".into(), date.trim().into());
                        } else {
                            note.push(line);
                        }
                    }
                    let note = note.join("\n");
                    if !note.trim().is_empty() {
                        r.extra.insert("note".into(), note.trim().into());
                    }
                }
                "date" => {
                    r.year = value.get(..4).and_then(|y| y.parse().ok()).filter(|&y| y != 0);
                    let date = value.split_whitespace().next().unwrap_or("").trim_end_matches("-00").trim_end_matches("-00");
                    if !date.is_empty() && date != "0000" {
                        r.extra.insert("date".into(), date.into());
                    }
                }
                _ => {
                    r.extra.insert(bib_field(&field).into(), value.into());
                }
            }
        }
        let mut others: BTreeMap<String, Vec<toml::Value>> = BTreeMap::new();
        let mut creators = db.prepare_cached(
            "SELECT c.firstName, c.lastName, c.fieldMode, ct.creatorType FROM itemCreators ic
             JOIN creators c USING(creatorID) JOIN creatorTypes ct USING(creatorTypeID)
             WHERE ic.itemID = ?1 ORDER BY ic.orderIndex",
        )?;
        let rows = creators.query_map(params![id], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?))
        })?;
        for row in rows {
            let (first, last, mode, role) = row?;
            let name = match first.filter(|f| mode == 0 && !f.is_empty()) {
                Some(f) => format!("{last}, {f}"),
                None => last,
            };
            if role == "author" {
                r.authors.push(name);
            } else {
                others.entry(role).or_default().push(name.into());
            }
        }
        for (role, names) in others {
            r.extra.insert(role, toml::Value::Array(names));
        }
        r.tags = query_strings(db, "SELECT t.name FROM itemTags it JOIN tags t USING(tagID) WHERE it.itemID = ?1 ORDER BY t.name", id)?;
        let mut stmt = db.prepare_cached("SELECT collectionID FROM collectionItems WHERE itemID = ?1")?;
        let ids: Vec<i64> = stmt.query_map(params![id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        r.collections = ids.iter().filter_map(|c| collections.get(c).cloned()).collect();
        r.collections.sort();

        let attachments = attachments(db, id, zotero, linked)?;
        let pdf_file = attachments.iter().find_map(|(_, path)| path.clone());
        if let Some(file) = &pdf_file {
            // Linked files can be anywhere: under the home folder, else under the root.
            match home.filter(|h| file.starts_with(h)) {
                Some(home) => ensure_location(&mut lib, &mut new_locations, "home", home),
                None => ensure_location(&mut lib, &mut new_locations, "files", file.ancestors().last().unwrap_or(file)),
            }
            r.pdf = locate(&lib, file);
        }
        if r.pdf.is_none() && attachments.iter().any(|(_, p)| p.is_none()) {
            sum.pdfs_unresolved += 1;
        }

        r.key = citation_key
            .map(|k| k.chars().filter(|c| c.is_ascii_alphanumeric() || "-_:.".contains(*c)).collect::<String>())
            .filter(|k| library::valid_key(k) && !lib.key_taken(k))
            .unwrap_or_else(|| lib.unique_key(&r));
        effects.push(lib.save(&r)?);

        let dir = library::annotations_dir(&r.key);
        let mut annots = Vec::new();
        for (att_id, file) in &attachments {
            import_annotations(db, *att_id, file.as_deref(), zotero, &dir, &mut annots, &mut effects, &mut sum, image)?;
        }
        let notes = format!(
            "SELECT i.key, n.note FROM itemNotes n JOIN items i USING(itemID) WHERE n.parentItemID = ?1 AND n.itemID {NOT_DELETED}"
        );
        let notes: Vec<String> = query_pairs(db, &notes, id)?.iter().map(|(_, note)| html_to_text(note)).collect();
        if !notes.is_empty() {
            sum.notes += notes.len();
            effects.push(Effect::write(library::note_path(&r.key), notes.join("\n\n---\n\n") + "\n"));
        }
        if !annots.is_empty() {
            effects.push(Effect::write(annotations::file_path(&r.key), annotations::render(annots)?));
        }
        lib.refs.push(r);
        sum.refs += 1;
    }

    let colors = tag_colors(db)?;
    let item_fields = query_all_strings(db, "SELECT fieldName FROM fieldsCombined")?;
    let mut searches = Vec::new();
    for (name, conditions) in saved_searches(db)? {
        match convert_search(&conditions, &collections_by_key, &item_fields) {
            Some(query) => searches.push((name, query)),
            None => sum.searches_skipped.push(name),
        }
    }
    effects.push(lib.update_config(|t| {
        let locations = config_section(t, "locations")?;
        for (name, base) in &new_locations {
            let shown = match home.map(|h| base.strip_prefix(h)) {
                Some(Ok(rest)) => format!("~/{}", rest.display()),
                _ => base.display().to_string(),
            };
            locations.insert(name.clone(), shown.trim_end_matches('/').into());
        }
        let tag_colors = config_section(t, "tag_colors")?;
        for (tag, color) in colors {
            if !tag_colors.contains_key(&tag) {
                tag_colors.insert(tag, color.into());
                sum.tag_colors += 1;
            }
        }
        let saved = config_section(t, "searches")?;
        for (name, query) in searches {
            if !saved.contains_key(&name) {
                saved.insert(name, toml::Table::from_iter([("query".to_string(), query.into())]).into());
                sum.searches += 1;
            }
        }
        Ok(())
    })?);

    let mut report = format!(
        "imported {} refs ({} already imported, skipped), {} annotations, {} notes, {} images ({} could not be rendered), {} items whose pdf could not be located, {} tag colors, {} saved searches",
        sum.refs, sum.skipped, sum.annotations, sum.notes, sum.images, sum.images_missing, sum.pdfs_unresolved, sum.tag_colors, sum.searches
    );
    if !sum.searches_skipped.is_empty() {
        report += &format!("\nsaved searches with conditions yeda cannot express (not imported): {}", sum.searches_skipped.join(", "));
    }
    Ok((report, effects))
}

fn tag_colors(db: &Connection) -> Result<Vec<(String, String)>> {
    let json: Vec<String> = query_all_strings(db, "SELECT value FROM syncedSettings WHERE setting = 'tagColors'")?;
    Ok(json
        .iter()
        .filter_map(|j| serde_json::from_str::<serde_json::Value>(j).ok())
        .flat_map(|v| v.as_array().cloned().unwrap_or_default())
        .filter_map(|c| Some((c["name"].as_str()?.to_string(), c["color"].as_str()?.to_string())))
        .collect())
}

type Condition = (String, String, String);

fn saved_searches(db: &Connection) -> Result<Vec<(String, Vec<Condition>)>> {
    let mut stmt = db.prepare("SELECT savedSearchID, savedSearchName FROM savedSearches ORDER BY savedSearchName")?;
    let searches: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut cond = db.prepare(
        "SELECT condition, COALESCE(operator, ''), COALESCE(value, '') FROM savedSearchConditions
         WHERE savedSearchID = ?1 ORDER BY searchConditionID",
    )?;
    searches
        .into_iter()
        .map(|(id, name)| {
            let conditions = cond.query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
            Ok((name, conditions))
        })
        .collect()
}

/// Translates Zotero search conditions into a yeda query; `None` if any condition has no equivalent.
fn convert_search(conditions: &[Condition], collections: &HashMap<String, String>, item_fields: &[String]) -> Option<String> {
    let recursive = conditions.iter().any(|(c, _, v)| c == "recursive" && v == "true");
    let mut terms = Vec::new();
    for (condition, op, value) in conditions {
        if value.contains('"') {
            return None;
        }
        let pattern = |v: &str| -> Option<String> {
            Some(match op.as_str() {
                "is" => format!("^{v}$"),
                "isNot" => format!("!^{v}$"),
                "contains" => format!("'{v}"),
                "doesNotContain" => format!("!'{v}"),
                "beginsWith" => format!("^{v}"),
                _ => return None,
            })
        };
        let term = match condition.as_str() {
            "resultLevel" | "noChildren" | "includeParentsAndChildren" | "recursive" => continue,
            "joinMode" if op == "all" || value == "all" => continue,
            "collection" => {
                let path = collections.get(value)?;
                let p = pattern(path)?;
                let p = if recursive { p.trim_end_matches('$').to_string() } else { p };
                format!("coll:\"{p}\"")
            }
            "extra" if value.starts_with("Read_Status") => {
                let status = value.strip_prefix("Read_Status").unwrap().trim_start_matches(':').trim();
                match (op.as_str(), status.is_empty()) {
                    ("doesNotContain", true) => "status:\"\"".into(),
                    ("contains" | "is", false) => format!("status:\"^{status}$\""),
                    ("doesNotContain" | "isNot", false) => format!("status:\"!^{status}$\""),
                    _ => return None,
                }
            }
            "tag" => format!("tag:\"{}\"", pattern(value)?),
            "creator" | "lastName" => format!("author:\"{}\"", pattern(value)?),
            "itemType" => format!("type:\"{}\"", pattern(bib_type(value))?),
            "quicksearch-titleCreatorYear" | "quicksearch-fields" | "anyField" => format!("\"{}\"", pattern(value)?),
            "DOI" => format!("doi:\"{}\"", pattern(&value.to_lowercase())?),
            field if item_fields.iter().any(|f| f == field) => {
                let name = match field {
                    "title" | "year" => field,
                    other => bib_field(other),
                };
                format!("{name}:\"{}\"", pattern(value)?)
            }
            _ => return None,
        };
        terms.push(term);
    }
    Some(terms.join(" "))
}

#[allow(clippy::too_many_arguments)]
fn import_annotations(
    db: &Connection,
    attachment: i64,
    pdf: Option<&Path>,
    zotero: &Path,
    dir: &str,
    out: &mut Vec<Annotation>,
    effects: &mut Vec<Effect>,
    sum: &mut Summary,
    image: &mut LoadImage,
) -> Result<()> {
    let mut stmt = db.prepare_cached(&format!(
        "SELECT i.key, a.type, a.text, a.comment, a.color, a.pageLabel, a.position, a.sortIndex FROM itemAnnotations a
         JOIN items i USING(itemID) WHERE a.parentItemID = ?1 AND a.itemID {NOT_DELETED} ORDER BY a.sortIndex"
    ))?;
    let rows = stmt.query_map(params![attachment], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    for row in rows {
        let (key, typ, text, comment, color, page, position, sort_index) = row?;
        let kind = match typ {
            1 => "highlight",
            2 => "note",
            5 => "underline",
            6 => "text",
            3 => "image",
            4 => "ink",
            _ => "other",
        };
        let mut a = Annotation {
            kind: kind.into(),
            page: page.unwrap_or_default(),
            color,
            text: text.filter(|t| !t.is_empty()),
            comment: comment.filter(|c| !c.is_empty()),
            source: "zotero".into(),
            sort_index,
            position: position_table(&position),
            ..Default::default()
        };
        if matches!(kind, "image" | "ink") {
            let source = ImageSource { cached: zotero.join("cache/library").join(format!("{key}.png")), pdf, area: image_rect(&position) };
            match (annotations::image_name(&key), image(&source)) {
                (Ok(name), Some(png)) => {
                    effects.push(Effect::Write { path: format!("{dir}/{name}"), bytes: png });
                    a.image = Some(name);
                    sum.images += 1;
                }
                _ => sum.images_missing += 1,
            }
        }
        a.id = key;
        out.push(a);
        sum.annotations += 1;
    }
    Ok(())
}

fn position_table(json: &str) -> Option<toml::Table> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    match toml::Value::try_from(v).ok()? {
        toml::Value::Table(t) => Some(t),
        _ => None,
    }
}

/// 1-based page and bounding rect from Zotero's `{"pageIndex": n, "rects": [[x0, y0, x1, y1]]}`.
fn image_rect(position: &str) -> Option<(u32, [f32; 4])> {
    let v: serde_json::Value = serde_json::from_str(position).ok()?;
    let r = v["rects"][0].as_array()?;
    let n = |i: usize| r.get(i)?.as_f64().map(|x| x as f32);
    Some((v["pageIndex"].as_u64()? as u32 + 1, [n(0)?, n(1)?, n(2)?, n(3)?]))
}

/// PDF attachments of `item` as (attachment id, resolved file); `None` when the file cannot be located.
fn attachments(db: &Connection, item: i64, zotero: &Path, linked: Option<&Path>) -> Result<Vec<(i64, Option<PathBuf>)>> {
    let mut stmt = db.prepare_cached(&format!(
        "SELECT a.itemID, i.key, a.linkMode, a.path FROM itemAttachments a JOIN items i USING(itemID)
         WHERE a.parentItemID = ?1 AND a.contentType = 'application/pdf' AND a.itemID {NOT_DELETED} ORDER BY a.itemID"
    ))?;
    let rows = stmt.query_map(params![item], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, Option<String>>(3)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, key, mode, path) = row?;
        let path = path.unwrap_or_default();
        // linkMode 0/1: stored in storage/<key>/; 2: linked file, absolute or relative to the base folder.
        let file = match (mode, path.strip_prefix("storage:"), path.strip_prefix("attachments:")) {
            (0 | 1, Some(name), _) => Some(zotero.join("storage").join(key).join(name)),
            (2, _, Some(rest)) => linked.map(|base| base.join(rest)),
            (2, None, None) if Path::new(&path).is_absolute() => Some(PathBuf::from(path)),
            _ => None,
        };
        out.push((id, file));
    }
    Ok(out)
}

/// Collection paths by collection id and by Zotero key.
fn collection_paths(db: &Connection) -> Result<(HashMap<i64, String>, HashMap<String, String>)> {
    let mut stmt = db.prepare("SELECT collectionID, collectionName, parentCollectionID, key FROM collections")?;
    let rows: Vec<(i64, String, Option<i64>, String)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let nodes: HashMap<i64, (String, Option<i64>)> = rows.iter().map(|(id, n, p, _)| (*id, (n.clone(), *p))).collect();
    let by_id: HashMap<i64, String> = nodes
        .keys()
        .map(|&id| {
            let mut parts = Vec::new();
            let mut cur = Some(id);
            while let Some((name, parent)) = cur.and_then(|c| nodes.get(&c)) {
                parts.push(name.replace('/', "-"));
                cur = *parent;
                if parts.len() > 64 {
                    break;
                }
            }
            parts.reverse();
            (id, parts.join("/"))
        })
        .collect();
    let by_key = rows.iter().map(|(id, _, _, key)| (key.clone(), by_id[id].clone())).collect();
    Ok((by_id, by_key))
}

fn query_pairs(db: &Connection, sql: &str, id: i64) -> Result<Vec<(String, String)>> {
    let mut stmt = db.prepare_cached(sql)?;
    let rows = stmt.query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn query_all_strings(db: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut stmt = db.prepare(sql)?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn query_strings(db: &Connection, sql: &str, id: i64) -> Result<Vec<String>> {
    let mut stmt = db.prepare_cached(sql)?;
    let rows = stmt.query_map(params![id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn locate(lib: &Library, file: &Path) -> Option<Pdf> {
    lib.locations
        .iter()
        .filter(|(_, base)| file.starts_with(base))
        .max_by_key(|(_, base)| base.components().count())
        .map(|(name, base)| Pdf { location: name.clone(), path: file.strip_prefix(base).unwrap().to_string_lossy().into() })
}

fn ensure_location(lib: &mut Library, new: &mut Vec<(String, PathBuf)>, name: &str, base: &Path) {
    if locate(lib, base).is_some() {
        return;
    }
    let name = std::iter::once(name.to_string())
        .chain((2..).map(|n| format!("{name}_{n}")))
        .find(|n| !lib.locations.contains_key(n))
        .unwrap();
    lib.locations.insert(name.clone(), base.into());
    new.push((name, base.into()));
}

fn bib_type(zotero: &str) -> &str {
    match zotero {
        "journalArticle" | "magazineArticle" | "newspaperArticle" => "article",
        "conferencePaper" => "inproceedings",
        "bookSection" => "incollection",
        "webpage" => "online",
        other => other,
    }
}

fn bib_field(zotero: &str) -> &str {
    match zotero {
        "publicationTitle" => "journal",
        "proceedingsTitle" | "bookTitle" => "booktitle",
        "issue" => "number",
        "place" => "address",
        "abstractNote" => "abstract",
        "university" => "school",
        "accessDate" => "urldate",
        "extra" => "note",
        "ISBN" => "isbn",
        "ISSN" => "issn",
        other => other,
    }
}

fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut tag = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match (in_tag, c) {
            (false, '<') => {
                in_tag = true;
                tag.clear();
            }
            (true, '>') => {
                in_tag = false;
                let name = tag.trim_start_matches('/').split(|c: char| !c.is_ascii_alphanumeric()).next().unwrap_or("");
                let block = matches!(name, "p" | "br" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
                if block && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            (true, c) => tag.push(c),
            (false, c) => out.push(c),
        }
    }
    annotations::unescape_html(out.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_note_html() {
        let html = r#"<div data-schema-version="9"><h1>Summary</h1><p>Cats &amp; dogs<br/>are <b>fluid</b>.</p></div>"#;
        assert_eq!(html_to_text(html), "Summary\nCats & dogs\nare fluid.");
    }

    #[test]
    fn finds_stored_and_linked_attachments() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE items (itemID INTEGER, key TEXT);
             CREATE TABLE itemAttachments (itemID INTEGER, parentItemID INTEGER, linkMode INTEGER, path TEXT, contentType TEXT);
             CREATE TABLE deletedItems (itemID INTEGER);
             INSERT INTO items VALUES (2, 'STORED'), (3, 'ABS'), (4, 'REL');
             INSERT INTO itemAttachments VALUES
               (2, 1, 1, 'storage:a.pdf', 'application/pdf'),
               (3, 1, 2, '/data/papers/b.pdf', 'application/pdf'),
               (4, 1, 2, 'attachments:sub/c.pdf', 'application/pdf');",
        )
        .unwrap();
        let files = |linked| attachments(&db, 1, Path::new("/z"), linked).unwrap().into_iter().map(|(_, f)| f).collect::<Vec<_>>();
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(files(Some(Path::new("/base"))), [p("/z/storage/STORED/a.pdf"), p("/data/papers/b.pdf"), p("/base/sub/c.pdf")]);
        assert_eq!(files(None)[2], None);
    }

    #[test]
    fn converts_saved_searches() {
        let colls = HashMap::from([("66C7TXJL".to_string(), "PhD Literature Review".to_string())]);
        let fields = vec!["title".to_string(), "publicationTitle".to_string()];
        let c = |a: &str, b: &str, v: &str| (a.to_string(), b.to_string(), v.to_string());
        let base = [c("resultLevel", "item", ""), c("collection", "is", "66C7TXJL")];
        let with = |extra: Condition| convert_search(&[base[0].clone(), base[1].clone(), extra], &colls, &fields);
        assert_eq!(
            with(c("extra", "contains", "Read_Status: To Read")).as_deref(),
            Some(r#"coll:"^PhD Literature Review$" status:"^To Read$""#)
        );
        assert_eq!(
            with(c("extra", "doesNotContain", "Read_Status")).as_deref(),
            Some(r#"coll:"^PhD Literature Review$" status:"""#)
        );
        assert_eq!(with(c("publicationTitle", "contains", "Nature")).as_deref(), Some(r#"coll:"^PhD Literature Review$" journal:"'Nature""#));
        assert_eq!(with(c("joinMode", "any", "")), None);
        assert_eq!(with(c("fulltextContent", "contains", "x")), None);
    }

    #[test]
    fn converts_position_json() {
        let t = position_table(r#"{"pageIndex":7,"rects":[[49.5,41.125,568.25,210.5]]}"#).unwrap();
        assert_eq!(t["pageIndex"].as_integer(), Some(7));
        assert_eq!(t["rects"][0][2].as_float(), Some(568.25));
    }

    #[test]
    fn parses_image_position() {
        let pos = r#"{"pageIndex":7,"rects":[[49.5,41.125,568.25,210.5]]}"#;
        assert_eq!(image_rect(pos), Some((8, [49.5, 41.125, 568.25, 210.5])));
    }
}
