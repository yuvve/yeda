use crate::annotations;
use crate::library::{Library, Ref, config_section};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Columns every table has, filled from the reference itself rather than by the LLM.
pub const BUILTIN: [&str; 4] = ["key", "title", "authors", "year"];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Table {
    pub columns: Vec<String>,
    /// Optional guidance per column, passed to the LLM.
    pub describe: BTreeMap<String, String>,
    /// Existing rows shown to the LLM as style examples (default 1).
    pub examples: Option<usize>,
}

pub type Row = BTreeMap<String, String>;

/// One paper's summarization request, prepared on the main thread so a worker only does I/O.
pub struct Job {
    pub key: String,
    pub columns: Vec<String>,
    pub system: String,
    pub text: String,
    pub schema: Value,
    pub pdf: Option<PathBuf>,
}

const SYSTEM: &str = "You fill in one row of a literature-review table about a research paper. \
Fill it objectively: report what the paper itself says and does. Make no assumptions about who will read \
the table, or about the researcher's own work, interests or plans. \
Each row stands on its own: describe this paper independently and never compare it with, rank it against, \
or refer to other papers in the table, including the example rows, which are style references only. \
The one exception is a column with existing values: reuse a value when it fits this paper closely. \
Where the paper does not say something, write \"Not stated.\" \
Follow each column's guidance when given; it takes precedence over the example rows. \
Otherwise match the style, depth and length of the example rows when there are any. Write plain text, not Markdown.";

/// `None` when the row has nothing left to fill (and `overwrite` is off).
pub fn plan(lib: &Library, name: &str, table: &Table, r: &Ref, overwrite: bool) -> Option<Job> {
    let existing = r.tables.get(name);
    let columns: Vec<String> = table
        .columns
        .iter()
        .filter(|c| overwrite || existing.and_then(|row| row.get(*c)).is_none_or(|v| v.trim().is_empty()))
        .cloned()
        .collect();
    if columns.is_empty() {
        return None;
    }
    Some(Job {
        key: r.key.clone(),
        system: SYSTEM.into(),
        text: prompt_text(lib, name, table, r, &columns),
        schema: schema(&columns),
        pdf: lib.pdf_path(r).ok().filter(|p| p.exists()),
        columns,
    })
}

fn prompt_text(lib: &Library, name: &str, table: &Table, r: &Ref, columns: &[String]) -> String {
    let mut s = String::from("Paper metadata:\n");
    let _ = writeln!(s, "title: {}\nauthors: {}", r.title, r.authors.join("; "));
    if let Some(y) = r.year {
        let _ = writeln!(s, "year: {y}");
    }
    if let Some(d) = &r.doi {
        let _ = writeln!(s, "doi: {d}");
    }
    for field in ["journal", "booktitle", "publisher", "volume", "number", "pages", "date", "url", "eprint"] {
        if let Some(v) = r.extra.get(field).and_then(|v| v.as_str()) {
            let _ = writeln!(s, "{field}: {v}");
        }
    }

    s += "\nColumns to fill:\n";
    for c in columns {
        let _ = write!(s, "- {c}");
        if let Some(d) = table.describe.get(c) {
            let _ = write!(s, ": {d}");
        }
        if let Some(values) = categories(lib, name, c) {
            let _ = write!(s, " (existing values: {}; reuse one when it fits closely, otherwise write a new one in the same style)", values.join("; "));
        }
        s.push('\n');
    }

    let examples = example_rows(lib, name, &r.key, columns, table.examples.unwrap_or(1));
    if !examples.is_empty() {
        s += "\nExample rows from this table, for style only (do not compare with or mention these papers):\n";
        for e in examples {
            s += &serde_json::to_string_pretty(&e).unwrap_or_default();
            s.push('\n');
        }
    }

    let annots = annotations::load(&lib.annotations_dir(&r.key)).unwrap_or_default();
    if !annots.is_empty() {
        s += "\nThe reader's highlights and notes on this paper:\n";
        for a in annots {
            let _ = write!(s, "- p.{} {}", a.page, a.kind);
            if let Some(t) = &a.text {
                let _ = write!(s, ": \"{t}\"");
            }
            if let Some(c) = &a.comment {
                let _ = write!(s, " (note: {c})");
            }
            s.push('\n');
        }
    }
    s
}

/// The distinct values of a column whose cells are short labels (some reused) rather than prose.
fn categories(lib: &Library, name: &str, column: &str) -> Option<Vec<String>> {
    let values: Vec<&String> = lib
        .refs
        .iter()
        .filter_map(|r| r.tables.get(name)?.get(column))
        .filter(|v| !v.trim().is_empty())
        .collect();
    let distinct: BTreeSet<&String> = values.iter().copied().collect();
    let labels = values.iter().all(|v| v.chars().count() <= 100 && !v.contains('\n'));
    let categorical = labels && distinct.len() <= 40 && values.len() > distinct.len();
    categorical.then(|| distinct.into_iter().cloned().collect())
}

fn example_rows(lib: &Library, name: &str, skip_key: &str, columns: &[String], n: usize) -> Vec<Value> {
    lib.refs
        .iter()
        .filter(|r| r.key != skip_key)
        .filter_map(|r| {
            let row = r.tables.get(name)?;
            let complete = columns.iter().all(|c| row.get(c).is_some_and(|v| !v.trim().is_empty()));
            complete.then(|| {
                let mut obj = serde_json::Map::new();
                obj.insert("title".into(), json!(r.title));
                for c in columns {
                    obj.insert(c.clone(), json!(row[c]));
                }
                Value::Object(obj)
            })
        })
        .take(n)
        .collect()
}

fn schema(columns: &[String]) -> Value {
    let properties: serde_json::Map<String, Value> = columns.iter().map(|c| (c.clone(), json!({"type": "string"}))).collect();
    json!({"type": "object", "properties": properties, "required": columns, "additionalProperties": false})
}

/// Reads the JSON object in an LLM reply; tolerates prose or code fences around it.
pub fn parse_row(reply: &str, columns: &[String]) -> Result<Row> {
    let start = reply.find('{').context("no JSON object in the model's reply")?;
    let end = reply.rfind('}').context("no JSON object in the model's reply")?;
    let obj: serde_json::Map<String, Value> =
        serde_json::from_str(&reply[start..=end]).context("the model's reply is not a valid JSON object")?;
    Ok(columns
        .iter()
        .filter_map(|c| {
            let v = obj.get(c)?;
            let s = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
            Some((c.clone(), s.trim().to_string()))
        })
        .collect())
}

/// Writes `row` into `r`'s row of table `name`; without `overwrite`, only empty cells are filled.
pub fn merge(r: &mut Ref, name: &str, row: Row, overwrite: bool) {
    let target = r.tables.entry(name.to_string()).or_default();
    for (c, v) in row {
        if overwrite || target.get(&c).is_none_or(|old| old.trim().is_empty()) {
            target.insert(c, v);
        }
    }
}

pub fn export_csv(lib: &Library, name: &str, path: &Path) -> Result<usize> {
    let table = lib.tables.get(name).with_context(|| format!("no table named '{name}'"))?;
    let mut w = csv::Writer::from_path(path).with_context(|| path.display().to_string())?;
    w.write_record(BUILTIN.iter().copied().chain(table.columns.iter().map(String::as_str)))?;
    let mut n = 0;
    for r in &lib.refs {
        let Some(row) = r.tables.get(name) else { continue };
        let builtin = [r.key.clone(), r.title.clone(), r.authors.join("; "), r.year.map(|y| y.to_string()).unwrap_or_default()];
        let cells = table.columns.iter().map(|c| row.get(c).cloned().unwrap_or_default());
        w.write_record(builtin.into_iter().chain(cells))?;
        n += 1;
    }
    w.flush()?;
    Ok(n)
}

fn normalize(title: &str) -> String {
    title.to_lowercase().chars().filter(char::is_ascii_alphanumeric).collect()
}

/// Imports rows from a CSV, matching references by key, then by title. Creates or extends the table.
pub fn import_csv(lib: &mut Library, name: &str, path: &Path) -> Result<String> {
    let mut rd = csv::Reader::from_path(path).with_context(|| path.display().to_string())?;
    let headers: Vec<String> = rd.headers()?.iter().map(String::from).collect();
    let columns: Vec<String> = headers.iter().filter(|h| !BUILTIN.contains(&h.as_str())).cloned().collect();
    let known = lib.tables.get(name).map(|t| t.columns.clone()).unwrap_or_default();
    let added: Vec<String> = columns.iter().filter(|c| !known.contains(c)).cloned().collect();
    if !added.is_empty() {
        lib.update_config(|t| {
            let def = config_section(config_section(t, "tables")?, name)?;
            let cols = def.entry("columns").or_insert_with(|| toml::Value::Array(vec![]));
            let cols = cols.as_array_mut().context("tables.<name>.columns is not an array")?;
            cols.extend(added.iter().map(|c| toml::Value::String(c.clone())));
            Ok(())
        })?;
    }

    let by_key: HashMap<String, usize> = lib.refs.iter().enumerate().map(|(i, r)| (r.key.clone(), i)).collect();
    let by_title: HashMap<String, usize> = lib.refs.iter().enumerate().map(|(i, r)| (normalize(&r.title), i)).collect();
    let col = |h: &str| headers.iter().position(|x| x == h);
    let (key_col, title_col) = (col("key"), col("title"));
    let (mut imported, mut unmatched) = (0, Vec::new());
    for record in rd.records() {
        let record = record?;
        let key = key_col.and_then(|i| record.get(i)).unwrap_or("");
        let title = title_col.and_then(|i| record.get(i)).unwrap_or("");
        let Some(&i) = by_key.get(key).or_else(|| by_title.get(&normalize(title)).filter(|_| !title.is_empty())) else {
            unmatched.push(if key.is_empty() { title.to_string() } else { key.to_string() });
            continue;
        };
        let row: Row = headers
            .iter()
            .zip(record.iter())
            .filter(|(h, v)| !BUILTIN.contains(&h.as_str()) && !v.trim().is_empty())
            .map(|(h, v)| (h.clone(), v.to_string()))
            .collect();
        merge(&mut lib.refs[i], name, row, true);
        lib.save(&lib.refs[i])?;
        imported += 1;
    }
    let mut report = format!("imported {imported} rows into table '{name}'");
    if !unmatched.is_empty() {
        let _ = write!(report, "; no matching reference for: {}", unmatched.join(", "));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_merges_rows() {
        let cols = vec!["method".to_string(), "hardware/sim".to_string()];
        let reply = "Here you go:\n```json\n{\"method\": \"CBBA\", \"hardware/sim\": \"ROS\", \"extra\": \"x\"}\n```";
        let row = parse_row(reply, &cols).unwrap();
        assert_eq!(row.len(), 2);
        assert_eq!(row["hardware/sim"], "ROS");

        let mut r = Ref::default();
        merge(&mut r, "t", Row::from([("method".into(), "mine".into())]), false);
        merge(&mut r, "t", row.clone(), false);
        assert_eq!(r.tables["t"]["method"], "mine");
        assert_eq!(r.tables["t"]["hardware/sim"], "ROS");
        merge(&mut r, "t", row, true);
        assert_eq!(r.tables["t"]["method"], "CBBA");
    }

    #[test]
    fn prompt_carries_column_guidance() {
        let dir = std::env::temp_dir().join(format!("yeda-tables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("refs")).unwrap();
        std::fs::write(
            dir.join("yeda.toml"),
            "[tables.lit]\ncolumns = [\"contribution\", \"method\"]\n\n[tables.lit.describe]\ncontribution = \"Most important. No jargon.\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("refs/a2020.toml"), "title = \"A\"\n").unwrap();
        let lib = Library::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let job = plan(&lib, "lit", &lib.tables["lit"], &lib.refs[0], false).unwrap();
        assert!(job.text.contains("- contribution: Most important. No jargon.\n- method\n"));
        assert!(job.system.contains("objectively"));
        assert!(job.system.contains("never compare"));
    }

    #[test]
    fn schema_requires_every_column() {
        let s = schema(&["a b".to_string()]);
        assert_eq!(s["required"][0], "a b");
        assert_eq!(s["additionalProperties"], false);
    }
}
