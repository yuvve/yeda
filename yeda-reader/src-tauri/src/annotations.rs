use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value as Json};
use toml::{Table, Value};

const FILE: &str = "annotations.toml";

/// Entries of `<dir>/annotations.toml` as JSON objects; empty if the file doesn't exist.
pub fn load(dir: &Path) -> Result<Vec<Json>, String> {
    let doc = read(dir)?;
    let entries = doc.get("annotation").and_then(Value::as_array).cloned().unwrap_or_default();
    entries.into_iter().map(|e| serde_json::to_value(e).map_err(|e| e.to_string())).collect()
}

/// Merges `changes` into the file by `id`: a key set to null is removed, other keys are replaced,
/// keys absent from a change are kept, and unknown ids are appended.
pub fn save(dir: &Path, changes: Vec<Map<String, Json>>) -> Result<(), String> {
    let mut doc = read(dir)?;
    let entries = annotation_array(&mut doc)?;
    for change in changes {
        let id = change.get("id").and_then(Json::as_str).ok_or("annotation without id")?.to_owned();
        let idx = entries.iter().position(|e| e.get("id").and_then(Value::as_str) == Some(&id));
        let idx = idx.unwrap_or_else(|| {
            entries.push(Value::Table(Table::new()));
            entries.len() - 1
        });
        let entry = entries[idx].as_table_mut().ok_or_else(|| format!("annotation {id} is not a table"))?;
        for (key, value) in change {
            match to_toml(value) {
                Some(v) => entry.insert(key, v),
                None => entry.remove(&key),
            };
        }
    }
    write(dir, &doc)
}

/// Removes the entries with these ids and their images.
pub fn delete(dir: &Path, ids: &[String]) -> Result<(), String> {
    let mut doc = read(dir)?;
    annotation_array(&mut doc)?
        .retain(|e| !e.get("id").and_then(Value::as_str).is_some_and(|id| ids.iter().any(|d| d == id)));
    write(dir, &doc)?;
    for id in ids {
        match fs::remove_file(image_path(dir, id)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.to_string()),
            _ => {}
        }
    }
    Ok(())
}

pub fn read_image(dir: &Path, id: &str) -> Result<Vec<u8>, String> {
    fs::read(image_path(dir, id)?).map_err(|e| e.to_string())
}

pub fn save_image(dir: &Path, id: &str, png: &[u8]) -> Result<(), String> {
    write_atomic(&image_path(dir, id)?, png).map_err(|e| e.to_string())
}

fn image_path(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("invalid annotation id: {id:?}"));
    }
    Ok(dir.join("img").join(format!("{id}.png")))
}

fn read(dir: &Path) -> Result<Table, String> {
    let path = dir.join(FILE);
    match fs::read_to_string(&path) {
        Ok(s) => s.parse().map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Table::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn write(dir: &Path, doc: &Table) -> Result<(), String> {
    let s = toml::to_string(doc).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(FILE), s.as_bytes()).map_err(|e| e.to_string())
}

fn annotation_array(doc: &mut Table) -> Result<&mut Vec<Value>, String> {
    doc.entry("annotation")
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
        .ok_or_else(|| "`annotation` is not an array".into())
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

/// TOML has no null, so nulls become `None`; inside arrays and tables they are dropped.
fn to_toml(v: Json) -> Option<Value> {
    Some(match v {
        Json::Null => return None,
        Json::Bool(b) => Value::Boolean(b),
        Json::Number(n) => n.as_i64().map(Value::Integer).unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => Value::String(s),
        Json::Array(a) => Value::Array(a.into_iter().filter_map(to_toml).collect()),
        Json::Object(o) => Value::Table(o.into_iter().filter_map(|(k, v)| Some((k, to_toml(v)?))).collect()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_by_id_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("yeda-reader-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(FILE),
            r#"
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
"#,
        )
        .unwrap();

        let change = |v: Json| v.as_object().unwrap().clone();
        save(
            &dir,
            vec![
                change(json!({ "id": "EDIT0001", "comment": "new comment", "text": null })),
                change(json!({ "id": "NEW00001", "kind": "ink", "page": "2", "source": "reader",
                    "position": { "pageIndex": 1, "paths": [[1.5, 2.0, 3.0, 4.0]], "width": 2.0 } })),
            ],
        )
        .unwrap();
        save_image(&dir, "NEW00001", b"png").unwrap();

        let loaded = load(&dir).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0]["pen_pressure"], json!([0.4, 0.6]));
        assert_eq!(loaded[1]["comment"], "new comment");
        assert!(loaded[1].get("text").is_none());
        assert_eq!(loaded[1]["position"]["rects"], json!([[49.5, 41.1, 568.2, 210.3]]));
        assert_eq!(loaded[2]["position"]["paths"], json!([[1.5, 2.0, 3.0, 4.0]]));
        assert_eq!(read_image(&dir, "NEW00001").unwrap(), b"png");

        let before = fs::read_to_string(dir.join(FILE)).unwrap();
        save(&dir, load(&dir).unwrap().into_iter().map(|v| v.as_object().unwrap().clone()).collect()).unwrap();
        assert_eq!(fs::read_to_string(dir.join(FILE)).unwrap(), before);

        delete(&dir, &["NEW00001".into()]).unwrap();
        assert_eq!(load(&dir).unwrap().len(), 2);
        assert!(!image_path(&dir, "NEW00001").unwrap().exists());
        assert!(image_path(&dir, "../escape").is_err());

        fs::remove_dir_all(&dir).unwrap();
    }
}
