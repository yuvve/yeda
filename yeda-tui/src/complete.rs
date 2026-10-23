use crate::app::{ACTIONS, COMMANDS};
use yeda_core::cite;
use yeda_core::library::Library;

/// Tab completion of a command line: the candidates, and the line before the word they replace.
pub struct Completion {
    pub head: String,
    pub candidates: Vec<String>,
    pub at: usize,
}

impl Completion {
    /// `None` when nothing completes the last word of `line`.
    pub fn new(lib: &Library, line: &str) -> Option<Completion> {
        let start = line.rfind(' ').map_or(0, |i| i + 1);
        let (head, word) = line.split_at(start);
        let lower = word.to_lowercase();
        let candidates: Vec<String> = options(lib, head).into_iter().filter(|c| c.to_lowercase().starts_with(&lower)).collect();
        (!candidates.is_empty()).then(|| Completion { head: head.to_string(), candidates, at: 0 })
    }

    pub fn line(&self) -> String {
        format!("{}{}", self.head, self.candidates[self.at])
    }

    pub fn step(&mut self, back: bool) {
        let n = self.candidates.len();
        self.at = if back { (self.at + n - 1) % n } else { (self.at + 1) % n };
    }
}

/// What the next word after `head` can be.
fn options(lib: &Library, head: &str) -> Vec<String> {
    let words: Vec<&str> = head.split_whitespace().collect();
    let Some((command, args)) = words.split_first() else { return command_names() };
    let tables = || lib.tables.keys().cloned().collect::<Vec<_>>();
    let locations = || lib.locations.keys().chain(lib.remotes.keys()).cloned().collect::<Vec<_>>();
    let mut options = match (command.trim_end_matches('!'), args.len()) {
        ("summarize", 0) | ("export", 0) => tables(),
        ("summarize", 1) => lib.llm.keys().cloned().collect(),
        ("table", 0) => [tables(), vec!["close".into(), "rename".into()]].concat(),
        ("table", 1) if args[0] == "rename" => tables(),
        ("graph", 0) => ["citations", "tags", "col", "close"].map(String::from).to_vec(),
        ("graph", 1) if args[0] == "col" => lib.tables.values().flat_map(|t| t.columns.clone()).collect(),
        ("fetch", 0) | ("attach", 1) => locations(),
        ("setup", 0) => ["llm", "openalex", "proxy", "table", "webdav"].map(String::from).to_vec(),
        ("setup", 1) if args[0] == "table" => tables(),
        ("config", 0) => ["device", "library"].map(String::from).to_vec(),
        ("setup", 1) if args[0] == "llm" => lib.llm.keys().cloned().collect(),
        ("setup", 1) if args[0] == "webdav" => lib.remotes.keys().cloned().collect(),
        ("cite", 0) => ["bibtex", "ident"].into_iter().chain(cite::style_names()).map(String::from).collect(),
        _ => Vec::new(),
    };
    options.sort();
    options.dedup();
    options
}

fn command_names() -> Vec<String> {
    let commands = COMMANDS.iter().filter_map(|(c, _)| c.strip_prefix(':')?.split([' ', '[']).next());
    let mut names: Vec<String> = commands.chain(ACTIONS.iter().map(|(a, _)| *a)).filter(|n| !n.starts_with('<')).map(String::from).collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_commands_and_arguments() {
        let cfg = "[tables.lit]\ncolumns = []\n[tables.lab]\ncolumns = []\n[llm.claude]\nprovider = \"anthropic\"\nmodel = \"m\"\n";
        let lib = Library::parse(Some(cfg), None, [], None).unwrap();
        let c = Completion::new(&lib, "summ").unwrap();
        assert_eq!(c.candidates, ["summarize"]);
        assert_eq!(Completion::new(&lib, "summarize! l").unwrap().candidates, ["lab", "lit"]);
        assert_eq!(Completion::new(&lib, "summarize lit ").unwrap().line(), "summarize lit claude");
        assert_eq!(Completion::new(&lib, "table ").unwrap().candidates, ["close", "lab", "lit", "rename"]);
        assert_eq!(Completion::new(&lib, "table rename l").unwrap().candidates, ["lab", "lit"]);
        assert!(Completion::new(&lib, "cite ie").unwrap().candidates.contains(&"ieee".to_string()));
        assert_eq!(Completion::new(&lib, "graph c").unwrap().candidates, ["citations", "close", "col"]);
        assert_eq!(Completion::new(&lib, "setup llm ").unwrap().candidates, ["claude"]);
        assert!(Completion::new(&lib, "zzz").is_none());
        assert!(Completion::new(&lib, "save x").is_none());
    }
}
