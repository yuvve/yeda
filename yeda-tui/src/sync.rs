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

pub enum Event {
    /// A line of git's output.
    Line(String),
    /// Files synced so far, of how many.
    Progress(usize, usize),
}

pub type Log<'a> = &'a (dyn Fn(Event) + Sync);

/// Runs git with its output going to `log`. The TUI stays on screen, so git must not ask for
/// anything: credentials come from a credential helper or ssh-agent, else the command fails.
fn streamed(dir: &Path, args: &[&str], log: Log) -> Result<bool> {
    log(Event::Line(format!("$ git {}", args.join(" "))));
    let mut command = git(dir);
    command.args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let mut child = command.spawn().context("git")?;
    let lines = |out: Box<dyn Read + Send>| {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            log(Event::Line(line));
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
const DATA: [&str; 3] = [".", ":(exclude,icase)*.pdf", ":(exclude)*.tmp"];

fn add_all(dir: &Path) -> Result<()> {
    stdout(dir, &[&["add", "-A", "--"][..], &DATA].concat()).map(drop)
}

fn require_repository(dir: &Path) -> Result<()> {
    if !own_repository(dir)? {
        bail!("the library folder is not a git repository of its own; :git-init [url] makes it one");
    }
    Ok(())
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

/// The files that differ between `range`'s two ends.
fn files(dir: &Path, range: &str) -> Result<std::collections::BTreeSet<String>> {
    Ok(stdout(dir, &["diff", "--name-only", range])?.lines().map(String::from).collect())
}

/// Commits everything, merges the upstream branch and pushes; returns what happened.
pub fn run(dir: &Path, log: Log) -> Result<String> {
    require_repository(dir)?;
    add_all(dir)?;
    if !output(dir, &["diff", "--cached", "--quiet"])?.status.success() {
        log(Event::Line(stdout(dir, &["commit", "-m", "yeda sync"])?.trim_end().to_string()));
    }
    if !streamed(dir, &["fetch"], log)? {
        bail!("git fetch failed; {NO_CREDENTIALS}");
    }
    if !output(dir, &["rev-parse", "--verify", "-q", "@{u}"])?.status.success() {
        bail!("the branch has no upstream; git push -u origin HEAD sets one");
    }
    let (incoming, outgoing) = (files(dir, "HEAD...@{u}")?, files(dir, "@{u}...HEAD")?);
    let total = incoming.union(&outgoing).count();
    log(Event::Progress(0, total));
    if !incoming.is_empty() && !streamed(dir, &["merge", "--no-edit", "@{u}"], log)? {
        let conflicts = stdout(dir, &["diff", "--name-only", "--diff-filter=U"])?;
        if conflicts.is_empty() {
            output(dir, &["merge", "--abort"])?;
            bail!("git merge failed");
        }
        // Left mid-merge, the next :sync would commit the conflict markers.
        if let Err(e) = resolve(dir, conflicts.lines()) {
            output(dir, &["merge", "--abort"])?;
            return Err(e);
        }
    }
    log(Event::Progress(incoming.len(), total));
    if stdout(dir, &["rev-parse", "HEAD"])? != stdout(dir, &["rev-parse", "@{u}"])? && !streamed(dir, &["push"], log)? {
        bail!("git push failed; {NO_CREDENTIALS}");
    }
    log(Event::Progress(total, total));
    Ok(match (incoming.len(), outgoing.len()) {
        (0, 0) => "synced: nothing changed".into(),
        (i, o) => format!("synced {total} files: {i} pulled, {o} pushed"),
    })
}

/// A file that differs from the last commit.
pub struct Change {
    /// A added, D deleted, M modified, or git's letter for anything else.
    pub status: char,
    pub path: String,
    untracked: bool,
}

/// Every change since the last commit, as `:sync` would commit it.
pub fn changes(dir: &Path) -> Result<Vec<Change>> {
    require_repository(dir)?;
    let tracked = stdout(dir, &[&["diff", "HEAD", "--name-status", "--no-renames", "-z", "--"][..], &DATA].concat())?;
    let untracked = stdout(dir, &[&["ls-files", "--others", "--exclude-standard", "-z", "--"][..], &DATA].concat())?;
    let mut fields = tracked.split_terminator('\0');
    let mut changes = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        changes.push(Change { status: status.chars().next().unwrap_or('M'), path: path.into(), untracked: false });
    }
    changes.extend(untracked.split_terminator('\0').map(|path| Change { status: 'A', path: path.into(), untracked: true }));
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

pub fn diff(dir: &Path, change: &Change) -> Result<String> {
    if change.untracked {
        // Exits 1 when the files differ, which they always do.
        let out = output(dir, &["diff", "--no-index", "--", "/dev/null", &change.path])?;
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    stdout(dir, &["diff", "HEAD", "--", &change.path])
}

/// Puts the files back as they were at the last commit; new files are deleted.
pub fn revert(dir: &Path, changes: &[&Change]) -> Result<()> {
    let paths = |untracked: bool| changes.iter().filter(|c| c.untracked == untracked).map(|c| c.path.as_str()).collect::<Vec<_>>();
    let (untracked, tracked) = (paths(true), paths(false));
    if !untracked.is_empty() {
        stdout(dir, &[&["clean", "-f", "--"][..], &untracked].concat())?;
    }
    if !tracked.is_empty() {
        stdout(dir, &[&["restore", "--source=HEAD", "--staged", "--worktree", "--"][..], &tracked].concat())?;
    }
    Ok(())
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
        let progress = std::sync::Mutex::new(None);
        run(&a, &|e| if let Event::Progress(done, total) = e { *progress.lock().unwrap() = Some((done, total)) }).unwrap();
        assert_eq!(*progress.lock().unwrap(), Some((1, 1)));
        assert!(run(&b, &|_| {}).is_err());
        assert!(!b.join(".git/MERGE_HEAD").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn revert_undoes_additions_deletions_and_edits() {
        let dir = std::env::temp_dir().join(format!("yeda-diff-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("refs")).unwrap();
        stdout(&dir, &["init", "-q"]).unwrap();
        stdout(&dir, &["config", "user.email", "t@t"]).unwrap();
        stdout(&dir, &["config", "user.name", "t"]).unwrap();
        for f in ["refs/a.toml", "refs/b.toml"] {
            std::fs::write(dir.join(f), "x\n").unwrap();
        }
        add_all(&dir).unwrap();
        stdout(&dir, &["commit", "-q", "-m", "base"]).unwrap();
        std::fs::write(dir.join("refs/a.toml"), "y\n").unwrap();
        std::fs::remove_file(dir.join("refs/b.toml")).unwrap();
        std::fs::write(dir.join("refs/c.toml"), "z\n").unwrap();
        std::fs::write(dir.join("refs/staged.toml"), "z\n").unwrap();
        std::fs::write(dir.join("paper.pdf"), "%PDF").unwrap();
        stdout(&dir, &["add", "refs/staged.toml"]).unwrap();
        let changes = changes(&dir).unwrap();
        let listed: Vec<_> = changes.iter().map(|c| format!("{} {}", c.status, c.path)).collect();
        assert_eq!(listed, ["M refs/a.toml", "D refs/b.toml", "A refs/c.toml", "A refs/staged.toml"]);
        assert!(diff(&dir, &changes[2]).unwrap().contains("+z"));
        revert(&dir, &changes.iter().collect::<Vec<_>>()).unwrap();
        assert!(super::changes(&dir).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("refs/a.toml")).unwrap(), "x\n");
        assert!(dir.join("refs/b.toml").exists() && !dir.join("refs/c.toml").exists() && !dir.join("refs/staged.toml").exists());
        assert!(dir.join("paper.pdf").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
