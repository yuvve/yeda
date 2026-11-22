//! `:discover` without arguments: papers, keywords and a description to search from, and the LLM to ask.

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
    Keyword(usize),
    AddKeyword,
    Description,
    Llm,
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Paper,
    Keyword,
    Description,
}

pub struct Paper {
    pub key: String,
    pub title: String,
    pub on: bool,
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
    pub keywords: Vec<String>,
    pub text: String,
    pub llms: Vec<String>,
    pub llm: usize,
    pub sel: usize,
    pub edit: Option<Edit>,
}

impl DiscoverForm {
    /// `offered` are the library's marked or selected references, listed but not used until switched on.
    pub fn new(lib: &Library, offered: &[&Ref], llm: Option<&str>) -> Self {
        let llms: Vec<String> = lib.llm.keys().cloned().collect();
        let wanted = llm.map(String::from).or_else(|| lib.default_llm.clone());
        let llm = wanted.and_then(|w| llms.iter().position(|l| *l == w)).unwrap_or(0);
        let papers = offered.iter().map(|r| Paper { key: r.key.clone(), title: r.title.clone(), on: false }).collect();
        let mut form = DiscoverForm { papers, keywords: Vec::new(), text: String::new(), llms, llm, sel: 0, edit: None };
        form.sel = form.rows().iter().position(|r| *r == Row::AddPaper).unwrap_or(0);
        form
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Header("Papers · space uses or skips")];
        rows.extend((0..self.papers.len()).map(Row::Paper));
        rows.push(Row::AddPaper);
        rows.push(Row::Header("Keywords"));
        rows.extend((0..self.keywords.len()).map(Row::Keyword));
        rows.push(Row::AddKeyword);
        rows.push(Row::Header("Description"));
        rows.push(Row::Description);
        rows.push(Row::Header("Model"));
        rows.push(Row::Llm);
        rows
    }

    pub fn used_papers(&self) -> Vec<&str> {
        self.papers.iter().filter(|p| p.on).map(|p| p.key.as_str()).collect()
    }

    pub fn llm_name(&self) -> Option<&str> {
        self.llms.get(self.llm).map(String::as_str)
    }

    /// What a search would send, for the line under the form.
    pub fn summary(&self) -> String {
        let (papers, words) = (self.used_papers().len(), self.keywords.len());
        let mut parts = Vec::new();
        if papers > 0 {
            parts.push(format!("{papers} paper{}", if papers == 1 { "" } else { "s" }));
        }
        if words > 0 {
            parts.push(format!("{words} keyword{}", if words == 1 { "" } else { "s" }));
        }
        if !self.text.trim().is_empty() {
            parts.push("description".into());
        }
        if parts.is_empty() {
            return "uses nothing yet".into();
        }
        parts.extend(self.llm_name().map(String::from));
        format!("uses {}", parts.join(" · "))
    }

    /// The library's references matching every word typed, best first by title order.
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
            Row::Keyword(_) | Row::AddKeyword => Some(Part::Keyword),
            Row::Description => Some(Part::Description),
            _ => None,
        }
    }

    fn start(&mut self, part: Part) {
        let buf = if part == Part::Description { self.text.clone() } else { String::new() };
        self.edit = Some(Edit { part, buf, back: 0, pick: 0 });
    }

    fn remove_here(&mut self) {
        match self.rows().get(self.sel).copied() {
            Some(Row::Paper(i)) => drop(self.papers.remove(i)),
            Some(Row::Keyword(i)) => drop(self.keywords.remove(i)),
            Some(Row::Description) => self.text.clear(),
            _ => {}
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
            KeyCode::Char(' ') => {
                if let Some(Row::Paper(i)) = self.rows().get(self.sel).copied() {
                    self.papers[i].on = !self.papers[i].on;
                }
            }
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
                let text = edit.buf.split_whitespace().collect::<Vec<_>>().join(" ");
                match edit.part {
                    Part::Description => self.text = text,
                    Part::Keyword if !text.is_empty() && !self.keywords.iter().any(|w| w.eq_ignore_ascii_case(&text)) => {
                        self.keywords.push(text);
                        self.sel = self.rows().iter().position(|r| *r == Row::AddKeyword).unwrap_or(self.sel);
                    }
                    Part::Paper => {
                        if let Some((key, title)) = matches.into_iter().nth(edit.pick) {
                            self.papers.push(Paper { key, title, on: true });
                            self.sel = self.rows().iter().position(|r| *r == Row::AddPaper).unwrap_or(self.sel);
                        }
                    }
                    Part::Keyword => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        Library::parse(Some("default_llm = \"b\"\n[llm.a]\nprovider = \"p\"\nmodel = \"m\"\n[llm.b]\nprovider = \"p\"\nmodel = \"m\"\n[providers.p]\nbase_url = \"https://x.org/v1\"\nprotocol = \"anthropic\"\nauth = \"oauth\"\n"), None, [], None).unwrap()
    }

    fn press(form: &mut DiscoverForm, refs: &[Ref], keys: &str) -> Vec<bool> {
        keys.chars().map(|c| matches!(form.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), refs), Outcome::Search { .. })).collect()
    }

    #[test]
    fn nothing_is_used_until_asked() {
        let lib = lib();
        let seed = Ref { key: "seed".into(), title: "Seed".into(), ..Default::default() };
        let mut form = DiscoverForm::new(&lib, &[&seed], None);
        assert_eq!(form.summary(), "uses nothing yet");
        assert_eq!(form.llm_name(), Some("b"));
        form.sel = 1;
        press(&mut form, &[], " ");
        assert_eq!(form.used_papers(), ["seed"]);
        assert_eq!(form.summary(), "uses 1 paper · b");
    }

    #[test]
    fn adds_a_keyword_and_a_paper_by_typing() {
        let lib = lib();
        let other = Ref { key: "doe2020".into(), title: "Swarm exploration".into(), ..Default::default() };
        let mut form = DiscoverForm::new(&lib, &[], Some("a"));
        assert_eq!(form.llm_name(), Some("a"));
        press(&mut form, std::slice::from_ref(&other), "aswarm");
        form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), std::slice::from_ref(&other));
        assert_eq!(form.used_papers(), ["doe2020"]);
        form.sel = form.rows().iter().position(|r| *r == Row::AddKeyword).unwrap();
        press(&mut form, &[], "arobots");
        form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &[]);
        assert_eq!(form.keywords, ["robots"]);
        assert_eq!(form.summary(), "uses 1 paper · 1 keyword · a");
        form.sel = form.rows().iter().position(|r| *r == Row::Keyword(0)).unwrap();
        press(&mut form, &[], "d");
        assert!(form.keywords.is_empty());
    }
}
