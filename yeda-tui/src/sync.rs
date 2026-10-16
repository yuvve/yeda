use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Output};
use yeda_core::annotations;

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    c
}

fn output(dir: &Path, args: &[&str]) -> Result<Output> {
    git(dir).args(args).output().context("git")
}

fn stdout(dir: &Path, args: &[&str]) -> Result<String> {
    let out = output(dir, args)?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs in the terminal so git can ask for credentials itself.
fn interactive(dir: &Path, args: &[&str]) -> Result<bool> {
    Ok(git(dir).args(args).status().context("git")?.success())
}

/// Commits everything, merges the upstream branch and pushes; returns what happened.
pub fn run(dir: &Path) -> Result<String> {
    // Conflict paths are relative to the repository root, which may be above the library.
    let root = std::path::PathBuf::from(stdout(dir, &["rev-parse", "--show-toplevel"])?.trim());
    let dir = root.as_path();
    let committed = !stdout(dir, &["status", "--porcelain"])?.is_empty();
    if committed {
        stdout(dir, &["add", "-A"])?;
        stdout(dir, &["commit", "-q", "-m", "yeda sync"])?;
    }
    if !interactive(dir, &["pull", "--no-rebase", "--no-edit"])? {
        let conflicts = stdout(dir, &["diff", "--name-only", "--diff-filter=U"])?;
        if conflicts.is_empty() {
            bail!("git pull failed");
        }
        resolve(dir, conflicts.lines())?;
    }
    if !interactive(dir, &["push", "-q"])? {
        bail!("git push failed");
    }
    Ok(if committed { "synced: committed local changes, pulled and pushed" } else { "synced: pulled and pushed" }.into())
}

/// Merges conflicted annotation files by id; any other conflict aborts the merge.
fn resolve<'a>(dir: &Path, conflicts: impl Iterator<Item = &'a str>) -> Result<()> {
    let (ours, others): (Vec<_>, Vec<_>) = conflicts.partition(|p| p.ends_with(annotations::FILE));
    if !others.is_empty() {
        stdout(dir, &["merge", "--abort"])?;
        bail!("merge aborted, conflicts in: {}; resolve them with git", others.join(", "));
    }
    for path in ours {
        let stage = |n: u8| -> Result<Option<String>> {
            let out = output(dir, &["show", &format!(":{n}:{path}")])?;
            Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
        };
        let (base, mine, theirs) = (stage(1)?, stage(2)?.unwrap_or_default(), stage(3)?.unwrap_or_default());
        let merged = annotations::merge3(base.as_deref(), &mine, &theirs).with_context(|| path.to_string())?;
        yeda_fs::write_atomic(&dir.join(path), merged.as_bytes())?;
        stdout(dir, &["add", "--", path])?;
    }
    stdout(dir, &["commit", "-q", "--no-edit"])?;
    Ok(())
}
