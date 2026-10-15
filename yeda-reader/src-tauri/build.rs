use std::path::Path;
use std::process::Command;

/// Builds the web frontend that `generate_context!` embeds, so a plain `cargo build` of this crate,
/// or of yeda-tui, which links it, needs no separate npm step.
fn main() {
    for path in ["../src", "../package.json", "../reader/build/web"] {
        println!("cargo:rerun-if-changed={path}");
    }
    if !Path::new("../../node_modules").exists() {
        npm(&["ci"], "../..");
    }
    if !Path::new("../reader/build/web").exists() {
        npm(&["run", "build:reader"], "..");
    }
    npm(&["run", "build"], "..");
    tauri_build::build()
}

fn npm(args: &[&str], dir: &str) {
    let status = Command::new("npm")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("running npm (install Node.js): {e}"));
    assert!(status.success(), "`npm {}` failed in {dir}", args.join(" "));
}
