fn main() {
    println!("cargo:rerun-if-changed=src/presets.toml");
    let presets = std::fs::read_to_string("src/presets.toml").expect("src/presets.toml");
    let text = format!(
        "# Providers\n\n\
         Generated from `src/presets.toml`; `:help providers` shows the same. Pick a preset with\n\
         `preset = \"<name>\"` under `[providers.<name>]` in `yeda.toml`. To change one, copy its\n\
         table into `yeda.toml` under a name of your own and edit it.\n\n\
         ```toml\n{presets}```\n"
    );
    if std::fs::read_to_string("PROVIDERS.md").ok().as_deref() != Some(&text) {
        std::fs::write("PROVIDERS.md", text).expect("write PROVIDERS.md");
    }
}
