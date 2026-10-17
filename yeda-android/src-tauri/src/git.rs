use anyhow::{Context, Result, bail};
use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{Commit, Cred, FetchOptions, IndexAddOption, PushOptions, RemoteCallbacks, Repository, Signature};
use std::cell::Cell;
use std::path::Path;
use yeda_core::annotations;

// ponytail: always the `origin` remote and the branch of the same name; read the upstream config if other setups appear.
const REMOTE: &str = "origin";

/// Credentials for HTTPS. `refused` is set when the server asked for a login and `token`
/// was missing or had already been tried, so the caller can ask for a new one.
pub struct Login<'a> {
    pub token: Option<&'a str>,
    pub refused: &'a Cell<bool>,
}

impl<'a> Login<'a> {
    fn callbacks(&self) -> RemoteCallbacks<'a> {
        let (token, refused) = (self.token, self.refused);
        let tried = Cell::new(false);
        let mut cb = RemoteCallbacks::new();
        cb.credentials(move |_, user, _| match token {
            Some(t) if !tried.replace(true) => Cred::userpass_plaintext(user.unwrap_or("git"), t),
            _ => {
                refused.set(true);
                Err(git2::Error::from_str("authentication required"))
            }
        });
        cb.push_update_reference(|name, status| match status {
            Some(why) => Err(git2::Error::from_str(&format!("push of {name} rejected: {why}"))),
            None => Ok(()),
        });
        cb
    }

    fn fetch_options(&self) -> FetchOptions<'a> {
        let mut o = FetchOptions::new();
        o.remote_callbacks(self.callbacks());
        o
    }
}

/// Android's OpenSSL build doesn't know where the system's CA certificates are.
pub fn init() {
    #[cfg(target_os = "android")]
    unsafe {
        let _ = git2::opts::set_ssl_cert_dir("/system/etc/security/cacerts");
    }
}

/// The URL of the library's remote, or `None` if the folder isn't itself a git repository.
pub fn remote_url(dir: &Path) -> Option<String> {
    let repo = Repository::open(dir).ok()?;
    repo.find_remote(REMOTE).ok()?.url().ok().map(String::from)
}

pub fn clone(url: &str, dir: &Path, login: &Login) -> Result<()> {
    RepoBuilder::new().fetch_options(login.fetch_options()).clone(url, dir).with_context(|| format!("cloning {url}"))?;
    Ok(())
}

fn signature(repo: &Repository) -> Result<Signature<'static>> {
    Ok(repo.signature().or_else(|_| Signature::now("yeda", "yeda@localhost"))?)
}

/// Commits every change in the working tree; false if there was none.
fn commit_all(repo: &Repository) -> Result<bool> {
    let mut index = repo.index()?;
    index.add_all(["*"], IndexAddOption::DEFAULT, None)?;
    index.update_all(["*"], None)?;
    index.write()?;
    let tree = index.write_tree()?;
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    if parent.as_ref().is_some_and(|p| p.tree_id() == tree) {
        return Ok(false);
    }
    let sig = signature(repo)?;
    repo.commit(Some("HEAD"), &sig, &sig, "yeda sync", &repo.find_tree(tree)?, &parent.iter().collect::<Vec<_>>())?;
    Ok(true)
}

/// Commits everything, merges `origin`'s branch of the same name and pushes; returns what happened.
pub fn sync(dir: &Path, login: &Login) -> Result<String> {
    // `open`, not `discover`: a parent repository (a dotfiles repo, say) must not be synced.
    let repo = Repository::open(dir).context("the library folder is not a git repository of its own")?;
    let committed = commit_all(&repo)?;
    let head = repo.head()?;
    let refname = head.name().context("HEAD is not a branch")?.to_string();
    let branch = head.shorthand().context("HEAD is not a branch")?.to_string();
    let mut remote = repo.find_remote(REMOTE)?;
    remote.fetch(&[&branch], Some(&mut login.fetch_options()), None)?;
    let ours = head.peel_to_commit()?;
    let theirs = match repo.find_reference(&format!("refs/remotes/{REMOTE}/{branch}")) {
        Ok(r) => Some(r.peel_to_commit()?),
        Err(e) if e.code() == git2::ErrorCode::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let mut pulled = false;
    if let Some(theirs) = &theirs {
        let (analysis, _) = repo.merge_analysis(&[&repo.find_annotated_commit(theirs.id())?])?;
        if analysis.is_fast_forward() {
            repo.find_reference(&refname)?.set_target(theirs.id(), "yeda sync: fast-forward")?;
            pulled = true;
        } else if analysis.is_normal() {
            merge(&repo, &ours, theirs, &branch)?;
            pulled = true;
        }
        // The working tree was committed above, so nothing local can be lost.
        repo.checkout_head(Some(CheckoutBuilder::new().force()))?;
    }
    let now = repo.head()?.peel_to_commit()?.id();
    let pushed = theirs.is_none_or(|t| t.id() != now);
    if pushed {
        let mut push = PushOptions::new();
        push.remote_callbacks(login.callbacks());
        remote.push(&[format!("{refname}:{refname}")], Some(&mut push))?;
    }
    let done: Vec<_> = [(committed, "committed"), (pulled, "pulled"), (pushed, "pushed")].iter().filter(|(d, _)| *d).map(|(_, w)| *w).collect();
    Ok(if done.is_empty() { "already up to date".into() } else { format!("synced: {}", done.join(", ")) })
}

/// Merges `theirs` into HEAD; conflicting annotation files are merged by id, any other conflict fails.
fn merge(repo: &Repository, ours: &Commit, theirs: &Commit, branch: &str) -> Result<()> {
    let mut index = repo.merge_commits(ours, theirs, None)?;
    let mut others = Vec::new();
    let conflicts = index.conflicts()?.collect::<Result<Vec<_>, _>>()?;
    for c in conflicts {
        let Some(path) = c.our.as_ref().or(c.their.as_ref()).map(|e| String::from_utf8_lossy(&e.path).into_owned()) else { continue };
        if !path.ends_with(annotations::FILE) {
            others.push(path);
            continue;
        }
        let text = |e: &Option<git2::IndexEntry>| -> Result<Option<String>> {
            e.as_ref().map(|e| Ok(String::from_utf8_lossy(repo.find_blob(e.id)?.content()).into_owned())).transpose()
        };
        let merged = annotations::merge3(text(&c.ancestor)?.as_deref(), &text(&c.our)?.unwrap_or_default(), &text(&c.their)?.unwrap_or_default())
            .with_context(|| path.clone())?;
        let mut entry = c.our.or(c.their).unwrap();
        entry.id = repo.blob(merged.as_bytes())?;
        entry.file_size = merged.len() as u32;
        entry.flags &= !0x3000; // the stage bits; 0 marks the entry resolved
        index.remove_path(Path::new(&path))?;
        index.add(&entry)?;
    }
    if !others.is_empty() {
        bail!("conflicts in {}; merge them on a computer with git", others.join(", "));
    }
    let tree = repo.find_tree(index.write_tree_to(repo)?)?;
    let sig = signature(repo)?;
    repo.commit(Some("HEAD"), &sig, &sig, &format!("Merge {REMOTE}/{branch} (yeda sync)"), &tree, &[ours, theirs])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success(), "git {args:?}");
    }

    fn append(dir: &Path, id: &str) {
        let path = dir.join("annotations/k/annotations.toml");
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text.push_str(&format!("[[annotation]]\nid = \"{id}\"\n"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn sync_merges_annotations_from_both_clones() {
        let t = std::env::temp_dir().join(format!("yeda-git-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(&t).unwrap();
        git(&t, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        let url = t.join("remote.git").display().to_string();
        let refused = Cell::new(false);
        let login = Login { token: None, refused: &refused };
        let (a, b) = (t.join("a"), t.join("b"));
        clone(&url, &a, &login).unwrap();
        append(&a, "A");
        git(&a, &["checkout", "-q", "-b", "main"]);
        assert_eq!(sync(&a, &login).unwrap(), "synced: committed, pushed");
        clone(&url, &b, &login).unwrap();
        append(&a, "B");
        append(&b, "C");
        sync(&a, &login).unwrap();
        assert_eq!(sync(&b, &login).unwrap(), "synced: committed, pulled, pushed");
        assert_eq!(sync(&a, &login).unwrap(), "synced: pulled");
        let ids = |dir: &Path| annotations::parse(&fs::read_to_string(dir.join("annotations/k/annotations.toml")).unwrap()).unwrap().into_iter().map(|a| a.id).collect::<Vec<_>>();
        assert_eq!(ids(&a), ["A", "C", "B"]);
        assert_eq!(ids(&b), ids(&a));
        assert!(!refused.get());
        // A folder inside a repository is not one of its own.
        fs::create_dir_all(a.join("inner")).unwrap();
        assert!(remote_url(&a).is_some() && remote_url(&a.join("inner")).is_none());
        assert!(sync(&a.join("inner"), &login).is_err());
        fs::remove_dir_all(&t).unwrap();
    }
}
