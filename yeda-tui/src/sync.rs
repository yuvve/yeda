use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Output, Stdio};
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

/// Where git's output goes as it runs, line by line.
pub type Log<'a> = &'a (dyn Fn(String) + Sync);

/// Runs git with its output going to `log`. The TUI stays on screen, so git must not ask for
/// anything: credentials come from a credential helper or ssh-agent, else the command fails.
fn streamed(dir: &Path, args: &[&str], log: Log) -> Result<bool> {
    log(format!("$ git {}", args.join(" ")));
    let mut command = git(dir);
    command.args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let mut child = command.spawn().context("git")?;
    let lines = |out: Box<dyn Read + Send>| {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            log(line);
        }
    };
    let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    std::thread::scope(|s| {
        s.spawn(|| lines(Box::new(stderr)));
        lines(Box::new(stdout));
    });
    Ok(child.wait().context("git")?.success())
}

const NO_CREDENTIALS: &str = "if it needs a password, set up a credential helper (gh auth setup-git for GitHub) or ssh-agent";

/// Whether the library folder is the top of a repository; one inside another repository
/// (dotfiles in the home folder, say) is not.
fn own_repository(dir: &Path) -> Result<bool> {
    let out = output(dir, &["rev-parse", "--show-toplevel"])?;
    let root = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Ok(out.status.success() && root.canonicalize()? == dir.canonicalize()?)
}

/// Git carries the data only: PDFs sync through WebDAV or Syncthing.
fn add_all(dir: &Path) -> Result<()> {
    stdout(dir, &["add", "-A", "--", ".", ":(exclude,icase)*.pdf", ":(exclude)*.tmp"]).map(drop)
}

/// Git carries no PDFs and none of yeda's atomic-write temporaries.
pub const GITIGNORE: &str = "*.tmp\n*.pdf\n*.PDF\n";
/// Syncthing must leave `.git` alone: it and git writing one repository corrupt it.
pub const STIGNORE: &str = ".git\n*.tmp\n";

/// Writes `path` unless it exists, so an ignore file the user wrote is kept.
pub fn write_new(path: &Path, contents: &str) -> Result<()> {
    if !path.exists() {
        yeda_fs::write_atomic(path, contents.as_bytes())?;
    }
    Ok(())
}

/// Makes the library folder a repository with everything committed; with `url`, adds it as
/// `origin` and pushes there, so `run` works afterwards.
pub fn init(dir: &Path, url: Option<&str>, log: Log) -> Result<String> {
    if own_repository(dir)? {
        bail!("the library folder is a git repository already");
    }
    stdout(dir, &["init", "-q"])?;
    write_new(&dir.join(".gitignore"), GITIGNORE)?;
    add_all(dir)?;
    stdout(dir, &["commit", "-q", "-m", "yeda library"])?;
    let Some(url) = url else { return Ok("git repository created; git remote add origin <url> connects it for :sync".into()) };
    stdout(dir, &["remote", "add", "origin", url])?;
    if !streamed(dir, &["push", "-u", "origin", "HEAD"], log)? {
        bail!("created the repository and added {url} as origin, but git push failed; {NO_CREDENTIALS}");
    }
    Ok(format!("git repository created and pushed to {url}"))
}

/// Commits everything, merges the upstream branch and pushes; returns what happened.
pub fn run(dir: &Path, log: Log) -> Result<String> {
    if !own_repository(dir)? {
        bail!("the library folder is not a git repository of its own; :git-init [url] makes it one");
    }
    add_all(dir)?;
    let committed = !output(dir, &["diff", "--cached", "--quiet"])?.status.success();
    if committed {
        log(stdout(dir, &["commit", "-m", "yeda sync"])?.trim_end().to_string());
    }
    if !streamed(dir, &["pull", "--no-rebase", "--no-edit"], log)? {
        let conflicts = stdout(dir, &["diff", "--name-only", "--diff-filter=U"])?;
        if conflicts.is_empty() {
            bail!("git pull failed; {NO_CREDENTIALS}");
        }
        // Left mid-merge, the next :sync would commit the conflict markers.
        if let Err(e) = resolve(dir, conflicts.lines()) {
            output(dir, &["merge", "--abort"])?;
            return Err(e);
        }
    }
    if !streamed(dir, &["push"], log)? {
        bail!("git push failed; {NO_CREDENTIALS}");
    }
    Ok(if committed { "synced: committed local changes, pulled and pushed" } else { "synced: pulled and pushed" }.into())
}

/// Merges conflicted annotation files by id; any other conflict aborts the merge.
fn resolve<'a>(dir: &Path, conflicts: impl Iterator<Item = &'a str>) -> Result<()> {
    let (ours, others): (Vec<_>, Vec<_>) = conflicts.partition(|p| p.ends_with(annotations::FILE));
    if !others.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_annotation_merge_leaves_no_merge_in_progress() {
        let root = std::env::temp_dir().join(format!("yeda-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        stdout(&root, &["init", "-q", "--bare", "remote.git"]).unwrap();
        let file = format!("annotations/a/{}", annotations::FILE);
        let write = |dir: &Path, text: &str| yeda_fs::write_atomic(&dir.join(&file), text.as_bytes()).unwrap();
        let clone = |name: &str| {
            stdout(&root, &["clone", "-q", "remote.git", name]).unwrap();
            let dir = root.join(name);
            stdout(&dir, &["config", "user.email", "t@t"]).unwrap();
            stdout(&dir, &["config", "user.name", "t"]).unwrap();
            dir
        };
        let a = clone("a");
        write(&a, "");
        add_all(&a).unwrap();
        stdout(&a, &["commit", "-q", "-m", "base"]).unwrap();
        stdout(&a, &["push", "-q", "-u", "origin", "HEAD"]).unwrap();
        let b = clone("b");
        write(&a, "[[annotation]]\nid = \"x\"\n");
        write(&b, "not toml [[\n");
        run(&a, &|_| {}).unwrap();
        assert!(run(&b, &|_| {}).is_err());
        assert!(!b.join(".git/MERGE_HEAD").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
