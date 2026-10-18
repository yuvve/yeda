use std::cmp::Ordering;
use yeda_core::library::{Library, Ref};
use yeda_core::search::Searcher;
use yeda_core::tables::BUILTIN;

/// A table of the references `:table` was run on, in the right column.
pub struct TableView {
    pub name: String,
    pub keys: Vec<String>,
    pub row: usize,
    pub col: usize,
    /// The first column on screen.
    pub left: usize,
    /// The column sorted by, and whether descending.
    pub sort: Option<(usize, bool)>,
    /// Words that must each match some cell of a row.
    pub query: String,
    /// Per column (by index), a value that column must match.
    pub filters: Vec<(usize, String)>,
    /// The selected cell is shown in full.
    pub open: bool,
    pub hidden: Vec<usize>,
}

impl TableView {
    pub fn new(name: String, keys: Vec<String>) -> Self {
        TableView { name, keys, row: 0, col: 0, left: 0, sort: None, query: String::new(), filters: Vec::new(), open: false, hidden: Vec::new() }
    }

    /// The indices of the columns not hidden, out of `n`.
    pub fn visible(&self, n: usize) -> Vec<usize> {
        (0..n).filter(|c| !self.hidden.contains(c)).collect()
    }

    /// Hides the selected column, unless it's the last one shown, and selects a neighbour.
    pub fn hide(&mut self, n: usize) {
        let vis = self.visible(n);
        if vis.len() < 2 {
            return;
        }
        self.hidden.push(self.col);
        self.col = vis.iter().copied().find(|&c| c > self.col).or(vis.iter().copied().rfind(|&c| c < self.col)).unwrap_or(0);
    }

    /// The cells of each row shown, filtered and sorted; the first is the reference key.
    pub fn rows(&self, lib: &Library, searcher: &mut Searcher) -> Vec<Vec<String>> {
        let columns = columns(lib, &self.name);
        let words: Vec<&str> = self.query.split_whitespace().collect();
        let mut rows: Vec<Vec<String>> = self
            .keys
            .iter()
            .filter_map(|k| lib.get(k))
            .map(|r| columns.iter().map(|c| cell(r, c)).collect::<Vec<_>>())
            .filter(|cells| {
                words.iter().all(|w| cells.iter().any(|c| searcher.fuzzy(w, c)))
                    && self.filters.iter().all(|(c, v)| cells.get(*c).is_some_and(|cell| matches(searcher, v, cell)))
            })
            .collect();
        if let Some((c, descending)) = self.sort {
            rows.sort_by(|a, b| compare(&a[c], &b[c], descending));
        }
        rows
    }
}

/// `value` matches fuzzily; `!value` doesn't match; `""` matches an empty cell.
fn matches(searcher: &mut Searcher, value: &str, cell: &str) -> bool {
    match value.strip_prefix('!') {
        _ if value == "\"\"" => cell.trim().is_empty(),
        Some(v) => !searcher.fuzzy(v, cell),
        None => searcher.fuzzy(value, cell),
    }
}

pub struct Column {
    /// `None` for the reference's own fields.
    pub table: Option<String>,
    pub name: String,
}

/// The reference's own fields, then the table's columns.
pub fn columns(lib: &Library, table: &str) -> Vec<Column> {
    let own = BUILTIN.iter().map(|c| Column { table: None, name: c.to_string() });
    let def = lib.tables.get(table).map(|t| t.columns.as_slice()).unwrap_or_default();
    own.chain(def.iter().map(|c| Column { table: Some(table.to_string()), name: c.clone() })).collect()
}

fn cell(r: &Ref, column: &Column) -> String {
    match (&column.table, column.name.as_str()) {
        (Some(t), c) => r.tables.get(t).and_then(|row| row.get(c)).cloned().unwrap_or_default(),
        (None, "key") => r.key.clone(),
        (None, "title") => r.title.clone(),
        (None, "authors") => r.authors.join("; "),
        (None, "year") => r.year.map(|y| y.to_string()).unwrap_or_default(),
        (None, _) => String::new(),
    }
}

/// Numbers by value, text ignoring case; empty cells go last either way.
fn compare(a: &str, b: &str, descending: bool) -> Ordering {
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    let order = match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.total_cmp(&y),
        _ => a.to_lowercase().cmp(&b.to_lowercase()),
    };
    if descending { order.reverse() } else { order }
}

/// The 1-based line of `column` under `[table]` in a file written by `Library::save_ai`.
pub fn line_of(text: &str, table: &str, column: &str) -> Option<usize> {
    let unquote = |s: &str| s.trim().trim_matches('"').to_string();
    let mut section = None;
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = Some(unquote(name));
        } else if section.as_deref() == Some(table) && line.split_once('=').is_some_and(|(k, _)| unquote(k) == column) {
            return Some(i + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_sorts_and_finds_cells() {
        let cfg = "[tables.lit]\ncolumns = [\"method\", \"hardware/sim\"]\n[tables.more]\ncolumns = [\"x\"]\n";
        let refs = [("a2020", "title = \"Alpha\"\nyear = 2020\n"), ("b2019", "title = \"Beta\"\nyear = 2019\n"), ("c2021", "title = \"Gamma\"\n")];
        let mut lib = Library::parse(Some(cfg), None, refs.map(|(k, t)| (k.to_string(), t.to_string())), None).unwrap();
        lib.load_ai("a2020", "[lit]\nmethod = \"CBBA\"\n\"hardware/sim\" = \"ROS\"\n").unwrap();
        lib.load_ai("b2019", "[lit]\nmethod = \"auction\"\n").unwrap();
        let names: Vec<String> = columns(&lib, "lit").into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["key", "title", "authors", "year", "method", "hardware/sim"]);
        let mut t = TableView::new("lit".into(), vec!["a2020".into(), "b2019".into(), "c2021".into()]);
        let mut s = Searcher::new();
        let keys = |t: &TableView, s: &mut Searcher| t.rows(&lib, s).into_iter().map(|r| r[0].clone()).collect::<Vec<_>>();

        t.sort = Some((3, false));
        assert_eq!(keys(&t, &mut s), ["b2019", "a2020", "c2021"]);
        t.sort = Some((3, true));
        assert_eq!(keys(&t, &mut s), ["a2020", "b2019", "c2021"]);
        t.query = "cbba".into();
        assert_eq!(keys(&t, &mut s), ["a2020"]);
        t.query.clear();
        t.filters = vec![(4, "!auction".into())];
        assert_eq!(keys(&t, &mut s), ["a2020", "c2021"]);
        t.filters = vec![(5, "\"\"".into())];
        assert_eq!(keys(&t, &mut s), ["b2019", "c2021"]);

        t.col = 5;
        t.hide(6);
        assert_eq!((t.visible(6), t.col), (vec![0, 1, 2, 3, 4], 4));
        t.hidden = vec![0, 1, 2, 3];
        t.hide(6);
        assert_eq!(t.visible(6), [5]);
        t.hide(6);
        assert_eq!(t.visible(6), [5]);

        let Ok(yeda_core::Effect::Write { bytes, .. }) = lib.save_ai(lib.get("a2020").unwrap()) else { panic!() };
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(line_of(&text, "lit", "hardware/sim").map(|n| text.lines().nth(n - 1).unwrap()), Some("\"hardware/sim\" = \"ROS\""));
        assert_eq!(line_of(&text, "other", "method"), None);
    }
}
