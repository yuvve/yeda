use std::path::Path;
use std::process::Command;

/// Builds the web frontend that `generate_context!` embeds, so a plain `cargo build` of this crate,
/// or of yeda, which links it, needs no separate npm step. A crate from crates.io has no frontend
/// sources, only the `dist/` built before publishing.
fn main() {
    if Path::new("../package.json").exists() {
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
    } else {
        assert!(Path::new("dist/index.html").exists(), "dist/ is missing: the crate was packaged without the built frontend");
    }
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
