//! `:discover` without arguments: the papers and the text to search from, and the LLM to ask.

use crate::app::edit_line;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yeda_core::library::{Library, Ref};

/// Papers listed while adding one.
const MATCHES: usize = 6;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Row {
    Header(&'static str),
    Paper(usize),
    AddPaper,
    Text,
    Llm,
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Paper,
    Text,
}

pub struct Paper {
    pub key: String,
    pub title: String,
}

/// What is being typed, and where it goes.
pub struct Edit {
    part: Part,
    pub buf: String,
    pub back: usize,
    pub pick: usize,
}

pub enum Outcome {
    Stay,
    Close,
    Search { fresh: bool },
}

pub struct DiscoverForm {
    pub papers: Vec<Paper>,
    pub text: String,
    pub llms: Vec<String>,
    pub llm: usize,
    /// Why the wanted LLM profile could not be preselected.
    pub llm_note: Option<String>,
    pub sel: usize,
    pub edit: Option<Edit>,
}

impl DiscoverForm {
    /// Starts with `papers`; the LLM is `llm`, else `default_llm`, else the first profile. A wrong
    /// name is only reported, since the form can still pick another.
    pub fn new(lib: &Library, papers: &[&Ref], llm: Option<&str>) -> Self {
        let llms: Vec<String> = lib.llm.keys().cloned().collect();
        let wanted = llm.or(lib.default_llm.as_deref());
        let at = wanted.and_then(|w| llms.iter().position(|l| l == w));
        let llm_note = match (wanted, at) {
            _ if llms.is_empty() => Some("no [llm.<name>] profile configured".to_string()),
            (Some(name), None) => Some(format!("{name} is not an llm profile")),
            _ => None,
        };
        let papers = papers.iter().map(|r| Paper { key: r.key.clone(), title: r.title.clone() }).collect();
        let mut form = DiscoverForm { papers, text: String::new(), llms, llm: at.unwrap_or(0), llm_note, sel: 0, edit: None };
        form.sel = form.rows().iter().position(|r| *r == Row::AddPaper).unwrap_or(0);
        form
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Header("Papers")];
        rows.extend((0..self.papers.len()).map(Row::Paper));
        rows.extend([Row::AddPaper, Row::Header("Looking for"), Row::Text, Row::Header("Model"), Row::Llm]);
        rows
    }

    pub fn llm_name(&self) -> Option<&str> {
        self.llms.get(self.llm).map(String::as_str)
    }

    /// What a search would send, for the line under the form.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match self.papers.len() {
            0 => {}
            1 => parts.push("1 paper".to_string()),
            n => parts.push(format!("{n} papers")),
        }
        if !self.text.trim().is_empty() {
            parts.push("text".into());
        }
        if parts.is_empty() {
            return "uses nothing yet".into();
        }
        parts.extend(self.llm_name().map(String::from));
        format!("uses {}", parts.join(" · "))
    }

    /// The library's references matching every word typed, in library order.
    pub fn matches<'a>(&self, refs: &'a [Ref]) -> Vec<&'a Ref> {
        let Some(edit) = self.edit.as_ref().filter(|e| e.part == Part::Paper) else { return Vec::new() };
        let words: Vec<String> = edit.buf.split_whitespace().map(str::to_lowercase).collect();
        refs.iter()
            .filter(|r| !self.papers.iter().any(|p| p.key == r.key))
            .filter(|r| {
                let hay = format!("{} {} {} {}", r.key, r.title, r.authors.join(" "), r.year.map(|y| y.to_string()).unwrap_or_default()).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .take(MATCHES)
            .collect()
    }

    fn selectable(row: &Row) -> bool {
        !matches!(row, Row::Header(_))
    }

    fn step(&mut self, down: bool) {
        let rows = self.rows();
        let mut i = self.sel;
        for _ in 0..rows.len() {
            i = if down { (i + 1) % rows.len() } else { (i + rows.len() - 1) % rows.len() };
            if Self::selectable(&rows[i]) {
                self.sel = i;
                return;
            }
        }
    }

    fn edge(&mut self, last: bool) {
        let rows = self.rows();
        let found = if last { rows.iter().rposition(Self::selectable) } else { rows.iter().position(Self::selectable) };
        self.sel = found.unwrap_or(0);
    }

    fn part_here(&self) -> Option<Part> {
        match self.rows().get(self.sel)? {
            Row::Paper(_) | Row::AddPaper => Some(Part::Paper),
            Row::Text => Some(Part::Text),
            _ => None,
        }
    }

    fn start(&mut self, part: Part) {
        let buf = if part == Part::Text { self.text.clone() } else { String::new() };
        self.edit = Some(Edit { part, buf, back: 0, pick: 0 });
    }

    fn remove_here(&mut self) {
        match self.rows().get(self.sel).copied() {
            Some(Row::Paper(i)) => drop(self.papers.remove(i)),
            Some(Row::Text) => self.text.clear(),
            _ => return,
        }
        let last = self.rows().len() - 1;
        self.sel = self.sel.min(last);
        if !Self::selectable(&self.rows()[self.sel]) {
            self.step(false);
        }
    }

    pub fn key(&mut self, k: KeyEvent, refs: &[Ref]) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.edit.is_some() {
            self.edit_key(k, ctrl, refs);
            return Outcome::Stay;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Enter => return Outcome::Search { fresh: false },
            KeyCode::Char('!') => return Outcome::Search { fresh: true },
            KeyCode::Char('j') | KeyCode::Down => self.step(true),
            KeyCode::Char('k') | KeyCode::Up => self.step(false),
            KeyCode::Char('g') | KeyCode::Home => self.edge(false),
            KeyCode::Char('G') | KeyCode::End => self.edge(true),
            KeyCode::Char('a' | 'e' | 'i') => {
                if let Some(part) = self.part_here() {
                    self.start(part);
                }
            }
            KeyCode::Char('d' | 'x') | KeyCode::Delete => self.remove_here(),
            KeyCode::Left | KeyCode::Char('h') if self.rows().get(self.sel) == Some(&Row::Llm) => self.cycle(false),
            KeyCode::Right | KeyCode::Char('l') if self.rows().get(self.sel) == Some(&Row::Llm) => self.cycle(true),
            _ => {}
        }
        Outcome::Stay
    }

    fn cycle(&mut self, forward: bool) {
        let n = self.llms.len();
        if n > 0 {
            self.llm = if forward { (self.llm + 1) % n } else { (self.llm + n - 1) % n };
            self.llm_note = None;
        }
    }

    fn edit_key(&mut self, k: KeyEvent, ctrl: bool, refs: &[Ref]) {
        let matches: Vec<(String, String)> = self.matches(refs).iter().map(|r| (r.key.clone(), r.title.clone())).collect();
        let Some(edit) = &mut self.edit else { return };
        if ctrl && matches!(k.code, KeyCode::Char('n' | 'p')) || matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab) {
            let n = matches.len();
            if n > 0 {
                let forward = matches!(k.code, KeyCode::Down | KeyCode::Tab) || k.code == KeyCode::Char('n');
                edit.pick = if forward { (edit.pick + 1) % n } else { (edit.pick + n - 1) % n };
            }
            return;
        }
        if edit_line(&mut edit.buf, &mut edit.back, k.code, ctrl) {
            edit.pick = 0;
            return;
        }
        match k.code {
            KeyCode::Esc => self.edit = None,
            KeyCode::Enter => {
                let Some(edit) = self.edit.take() else { return };
                match edit.part {
                    Part::Text => self.text = edit.buf.split_whitespace().collect::<Vec<_>>().join(" "),
                    Part::Paper => {
                        if let Some((key, title)) = matches.into_iter().nth(edit.pick) {
                            self.papers.push(Paper { key, title });
                            self.sel = self.rows().iter().position(|r| *r == Row::AddPaper).unwrap_or(self.sel);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "default_llm = \"b\"\n[llm.a]\nprovider = \"p\"\nmodel = \"m\"\n[llm.b]\nprovider = \"p\"\nmodel = \"m\"\n[providers.p]\nprotocol = \"anthropic\"\nauth = \"oauth\"\n";

    fn press(form: &mut DiscoverForm, refs: &[Ref], keys: &str) {
        for c in keys.chars() {
            form.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), refs);
        }
    }

    fn enter(form: &mut DiscoverForm, refs: &[Ref]) {
        form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), refs);
    }

    #[test]
    fn starts_with_the_given_papers_and_the_default_llm() {
        let lib = Library::parse(Some(CONFIG), None, [], None).unwrap();
        let seed = Ref { key: "seed".into(), title: "Seed".into(), ..Default::default() };
        let form = DiscoverForm::new(&lib, &[&seed], None);
        assert_eq!((form.summary().as_str(), form.llm_name(), form.llm_note.as_deref()), ("uses 1 paper · b", Some("b"), None));
        assert_eq!(DiscoverForm::new(&lib, &[], None).summary(), "uses nothing yet");
    }

    #[test]
    fn a_wrong_default_llm_still_opens_the_form() {
        let lib = Library::parse(Some(&CONFIG.replace("default_llm = \"b\"", "default_llm = \"gone\"")), None, [], None).unwrap();
        let mut form = DiscoverForm::new(&lib, &[], None);
        assert_eq!((form.llm_name(), form.llm_note.as_deref()), (Some("a"), Some("gone is not an llm profile")));
        form.sel = form.rows().iter().position(|r| *r == Row::Llm).unwrap();
        form.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &[]);
        assert_eq!((form.llm_name(), form.llm_note.as_deref()), (Some("b"), None));
        let none = DiscoverForm::new(&Library::parse(None, None, [], None).unwrap(), &[], Some("x"));
        assert_eq!((none.llm_name(), none.llm_note.as_deref()), (None, Some("no [llm.<name>] profile configured")));
    }

    #[test]
    fn adds_and_removes_a_paper_and_writes_the_text() {
        let lib = Library::parse(Some(CONFIG), None, [], None).unwrap();
        let other = Ref { key: "doe2020".into(), title: "Swarm exploration".into(), ..Default::default() };
        let refs = std::slice::from_ref(&other);
        let mut form = DiscoverForm::new(&lib, &[], Some("a"));
        assert_eq!(form.llm_name(), Some("a"));
        press(&mut form, refs, "aswarm");
        enter(&mut form, refs);
        assert_eq!(form.papers.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), ["doe2020"]);
        form.sel = form.rows().iter().position(|r| *r == Row::Text).unwrap();
        press(&mut form, &[], "arobots  decentralized");
        enter(&mut form, &[]);
        assert_eq!(form.summary(), "uses 1 paper · text · a");
        assert_eq!(form.text, "robots decentralized");
        form.sel = form.rows().iter().position(|r| *r == Row::Paper(0)).unwrap();
        press(&mut form, &[], "d");
        assert!(form.papers.is_empty());
    }
}
