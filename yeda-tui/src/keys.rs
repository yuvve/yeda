use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub const DEFAULTS: &str = include_str!("keys.toml");
pub const CONTEXTS: [&str; 6] = ["normal", "library", "graph", "cluster", "table", "annotations"];

/// A key sequence and the command line it runs.
pub type Binding = (Vec<String>, String);

/// The bindings of each context, in the order they were defined.
pub struct Keymap {
    contexts: Vec<(String, Vec<Binding>)>,
}

pub enum Lookup {
    Run(String),
    /// The keys so far start a longer binding.
    Prefix,
    None,
}

impl Keymap {
    /// The defaults with `[keys]` from the device config (`user`) applied on top.
    pub fn load(user: Option<&str>) -> Result<Keymap> {
        let mut map = Keymap { contexts: CONTEXTS.iter().map(|c| (c.to_string(), Vec::new())).collect() };
        map.apply(DEFAULTS).context("built-in key bindings")?;
        if let Some(user) = user {
            map.apply(user).context("[keys] in config.toml")?;
        }
        Ok(map)
    }

    fn apply(&mut self, text: &str) -> Result<()> {
        let table: toml::Table = toml::from_str(text)?;
        let Some(keys) = table.get("keys") else { return Ok(()) };
        let keys = keys.as_table().context("keys must be a table")?;
        for (context, bindings) in keys {
            let Some((_, list)) = self.contexts.iter_mut().find(|(c, _)| c == context) else {
                anyhow::bail!("unknown context [keys.{context}]; use one of {}", CONTEXTS.join(", "));
            };
            for (keys, command) in bindings.as_table().with_context(|| format!("[keys.{context}] must be a table"))? {
                let command = command.as_str().with_context(|| format!("{keys}: a binding is a command line in quotes"))?;
                let seq: Vec<String> = keys.split_whitespace().map(normalize).collect();
                list.retain(|(s, _)| *s != seq);
                if !command.is_empty() {
                    list.push((seq, command.to_string()));
                }
            }
        }
        Ok(())
    }

    /// What `seq` does in `context`, which falls back to `normal`.
    pub fn lookup(&self, context: &str, seq: &[String]) -> Lookup {
        let mut prefix = false;
        for c in [context, "normal"] {
            for (s, command) in self.bindings(c) {
                if s.as_slice() == seq {
                    return Lookup::Run(command.clone());
                }
                prefix |= s.len() > seq.len() && s.starts_with(seq);
            }
        }
        if prefix { Lookup::Prefix } else { Lookup::None }
    }

    pub fn bindings(&self, context: &str) -> &[Binding] {
        self.contexts.iter().find(|(c, _)| c == context).map_or(&[], |(_, l)| l.as_slice())
    }
}

/// `Ctrl-D` and `ctrl-d` are the same key; letters keep their case, which is the shift.
fn normalize(key: &str) -> String {
    let mut rest = key;
    let mut out = String::new();
    for m in ["ctrl-", "alt-"] {
        if rest.len() > m.len() && rest[..m.len()].eq_ignore_ascii_case(m) {
            out += m;
            rest = &rest[m.len()..];
        }
    }
    out + rest
}

/// The name a key has in bindings, such as `j`, `G`, `ctrl-d`, `Enter` or `Space`.
pub fn name(k: &KeyEvent) -> Option<String> {
    let base = match k.code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "shift-Tab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::F(n) => format!("F{n}"),
        _ => return None,
    };
    let mut out = String::new();
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        out += "ctrl-";
    }
    if k.modifiers.contains(KeyModifiers::ALT) {
        out += "alt-";
    }
    Some(out + &base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn overrides_unbinds_and_falls_back() {
        let map = Keymap::load(Some("[keys.normal]\n\"x\" = \"\"\n\"Ctrl-G\" = \"cite ieee\"\n[keys.graph]\n\"j\" = \"graph-scroll-down\"")).unwrap();
        assert!(matches!(map.lookup("normal", &seq("x")), Lookup::None));
        assert!(matches!(map.lookup("normal", &seq("ctrl-G")), Lookup::Run(c) if c == "cite ieee"));
        assert!(matches!(map.lookup("graph", &seq("j")), Lookup::Run(c) if c == "graph-scroll-down"));
        assert!(matches!(map.lookup("graph", &seq("k")), Lookup::Run(c) if c == "up"));
        assert!(matches!(map.lookup("library", &seq("g")), Lookup::Prefix));
        assert!(matches!(map.lookup("library", &seq("g g")), Lookup::Run(c) if c == "top"));
        assert!(Keymap::load(Some("[keys.nowhere]\n")).is_err());
    }

    #[test]
    fn defaults_run_known_commands() {
        let map = Keymap::load(None).unwrap();
        let commands: Vec<&str> = crate::app::COMMANDS.iter().filter_map(|(c, _)| c.trim_start_matches(':').split([' ', '[', '!']).next()).collect();
        for context in CONTEXTS {
            for (seq, command) in map.bindings(context) {
                let name = command.split_whitespace().next().unwrap();
                let known = crate::app::ACTIONS.iter().any(|(a, _)| *a == name) || commands.contains(&name) || name == "quit";
                assert!(known, "{seq:?} in {context} runs unknown {command}");
            }
        }
    }

    #[test]
    fn names_keys() {
        let k = |code, m| name(&KeyEvent::new(code, m)).unwrap();
        assert_eq!(k(KeyCode::Char('d'), KeyModifiers::CONTROL), "ctrl-d");
        assert_eq!(k(KeyCode::Char('G'), KeyModifiers::SHIFT), "G");
        assert_eq!(k(KeyCode::Char(' '), KeyModifiers::NONE), "Space");
    }
}
