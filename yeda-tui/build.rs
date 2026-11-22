#[path = "src/keydoc.rs"]
mod keydoc;

const CONTEXTS: [&str; 6] = ["normal", "library", "graph", "cluster", "table", "annotations"];

fn main() {
    println!("cargo:rerun-if-changed=src/keydoc.rs");
    println!("cargo:rerun-if-changed=src/keys.toml");
    let defaults: toml::Table = toml::from_str(include_str!("src/keys.toml")).expect("keys.toml");
    let keys = defaults["keys"].as_table().expect("[keys]");
    let contexts: Vec<(&str, keydoc::Bindings)> = CONTEXTS
        .iter()
        .map(|context| {
            let bindings = keys.get(*context).and_then(|t| t.as_table()).into_iter().flatten();
            (*context, bindings.map(|(k, c)| (k.split_whitespace().collect(), c.as_str().expect("command").to_string())).collect())
        })
        .collect();
    let text = keydoc::markdown(&keydoc::rows(&contexts));
    if std::fs::read_to_string("KEYS.md").ok().as_deref() != Some(&text) {
        std::fs::write("KEYS.md", text).expect("write KEYS.md");
    }
}
