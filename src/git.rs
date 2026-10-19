//! The git side of race mode: find the repo, add and remove lane
//! worktrees, and measure what each lane changed.
//!
//! Agents often leave their work uncommitted and add new files, so "what a
//! lane changed" is its working tree, untracked files included, against the
//! race's base commit. That is computed through a throwaway copy of the
//! lane's index, never touching the real one.

use crate::lanes::Repo;
use crate::state;
use std::path::Path;
use std::process::{Command, Stdio};

fn git(dir: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("git {}: {}", args[0], err.trim()))
    }
}

/// The repo `cwd` is in, or None outside git (and in a repo without
/// commits, which has nothing to branch from).
pub fn repo(cwd: &str) -> Option<Repo> {
    let common = git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok()?;
    let base = git(cwd, &["rev-parse", "HEAD"]).ok()?;
    let common = Path::new(&common);
    // The main checkout owns `.git`; a bare-ish layout falls back to toplevel.
    let root = match common.file_name().and_then(|n| n.to_str()) {
        Some(".git") => common.parent()?.to_string_lossy().into_owned(),
        _ => git(cwd, &["rev-parse", "--show-toplevel"]).ok()?,
    };
    let name = Path::new(&root).file_name()?.to_string_lossy().into_owned();
    Some(Repo { root, name, base })
}

pub fn worktree_add(repo: &Repo, path: &str, branch: &str) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    git(&repo.root, &["worktree", "add", "-q", "-b", branch, path, &repo.base]).map(|_| ())
}

/// Remove a lane's checkout, uncommitted work and all. Already gone is fine.
pub fn worktree_remove(repo: &Repo, path: &str) -> Result<(), String> {
    if !Path::new(path).exists() {
        let _ = git(&repo.root, &["worktree", "prune"]);
        return Ok(());
    }
    git(&repo.root, &["worktree", "remove", "--force", path]).map(|_| ())
}

pub fn branch_delete(repo: &Repo, branch: &str) -> Result<(), String> {
    match git(&repo.root, &["branch", "-D", branch]) {
        Err(e) if e.contains("not found") => Ok(()),
        r => r.map(|_| ()),
    }
}

pub fn branch_exists(repo: &Repo, branch: &str) -> bool {
    git(&repo.root, &["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")]).is_ok()
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stat {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

impl Stat {
    pub fn describe(&self) -> String {
        if self.files == 0 {
            return "no changes".into();
        }
        format!(
            "{} file{} +{} −{}",
            self.files,
            if self.files == 1 { "" } else { "s" },
            self.added,
            self.removed
        )
    }
}

/// Sum `git diff --numstat`; binary files count as a file with no lines.
pub fn parse_numstat(out: &str) -> Stat {
    let mut s = Stat::default();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.split('\t');
        let a = parts.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        let r = parts.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        s.files += 1;
        s.added += a;
        s.removed += r;
    }
    s
}

/// A copy of the lane's index with every working-tree change staged, so
/// `diff --cached <base>` covers commits, edits and new files. Copying the
/// real index (instead of reading HEAD into a fresh one) keeps its stat
/// cache, so `add -A` does not rehash the whole tree.
struct TempIndex(std::path::PathBuf);

impl TempIndex {
    fn new(dir: &str) -> Result<Self, String> {
        let real = git(dir, &["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
        let tmp = std::env::temp_dir().join(format!(
            "derby-index-{}-{:.0}",
            std::process::id(),
            state::now_secs() * 1e6
        ));
        if std::fs::copy(&real, &tmp).is_err() {
            Command::new("git")
                .args(["-C", dir, "read-tree", "HEAD"])
                .env("GIT_INDEX_FILE", &tmp)
                .status()
                .map_err(|e| e.to_string())?;
        }
        let ok = Command::new("git")
            .args(["-C", dir, "add", "-A"])
            .env("GIT_INDEX_FILE", &tmp)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if !ok {
            let _ = std::fs::remove_file(&tmp);
            return Err("git add -A failed".into());
        }
        Ok(TempIndex(tmp))
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn diff_stat(dir: &str, base: &str) -> Result<Stat, String> {
    let idx = TempIndex::new(dir)?;
    let out = Command::new("git")
        .args(["-C", dir, "diff", "--cached", "--numstat", base])
        .env("GIT_INDEX_FILE", &idx.0)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(parse_numstat(&String::from_utf8_lossy(&out.stdout)))
}

/// Show the lane's full diff in git's own pager (the user's pager config,
/// delta and all), on the inherited terminal.
pub fn show_diff(dir: &str, base: &str) -> Result<(), String> {
    let idx = TempIndex::new(dir)?;
    Command::new("git")
        .args(["-C", dir, "-p", "diff", "--cached", base])
        .env("GIT_INDEX_FILE", &idx.0)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// What a lane committed on its branch, for a lane whose worktree is gone.
pub fn branch_stat(repo: &Repo, branch: &str) -> Result<Stat, String> {
    git(&repo.root, &["diff", "--numstat", &repo.base, branch]).map(|out| parse_numstat(&out))
}

pub fn show_branch_diff(repo: &Repo, branch: &str) -> Result<(), String> {
    Command::new("git")
        .args(["-C", &repo.root, "-p", "diff", &repo.base, branch])
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_sums_and_counts_binaries() {
        let s = parse_numstat("12\t3\tsrc/a.rs\n-\t-\tlogo.png\n0\t7\told.txt\n");
        assert_eq!(
            s,
            Stat {
                files: 3,
                added: 12,
                removed: 10
            }
        );
        assert_eq!(s.describe(), "3 files +12 −10");
        assert_eq!(parse_numstat("").describe(), "no changes");
    }

    /// End to end on a scratch repo: a worktree lane with a commit, an edit
    /// and a new file, measured against the base; the real index untouched.
    #[test]
    fn worktree_lane_round_trip() {
        let dir = std::env::temp_dir().join(format!("derby-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.join("repo");
        let d = d.to_str().unwrap();
        let sh = |cmd: &str| {
            let ok = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap()
                .success();
            assert!(ok, "{cmd}");
        };
        sh(&format!(
            "git init -q {d} && cd {d} && printf 'a\\nb\\n' > a.txt && git add . && git commit -qm init"
        ));
        let r = repo(d).unwrap();
        assert_eq!(r.name, "repo");
        let wt = dir.join("lane").to_string_lossy().into_owned();
        worktree_add(&r, &wt, "race/t/claude-1").unwrap();
        // The lane's repo resolves to the main checkout.
        assert_eq!(repo(&wt).unwrap().root, r.root);
        sh(&format!(
            "cd {wt} && echo c >> a.txt && git commit -qam two && echo d >> a.txt && echo new > n.txt"
        ));
        let s = diff_stat(&wt, &r.base).unwrap();
        assert_eq!(s, Stat { files: 2, added: 3, removed: 0 });
        // n.txt is still untracked in the real index.
        let status = git(&wt, &["status", "--porcelain"]).unwrap();
        assert!(status.contains("?? n.txt"), "{status}");
        // Deleted by hand: the branch still tells what was committed.
        std::fs::remove_dir_all(&wt).unwrap();
        assert!(branch_exists(&r, "race/t/claude-1"));
        let s = branch_stat(&r, "race/t/claude-1").unwrap();
        assert_eq!(s, Stat { files: 1, added: 1, removed: 0 });
        worktree_remove(&r, &wt).unwrap();
        branch_delete(&r, "race/t/claude-1").unwrap();
        assert!(!branch_exists(&r, "race/t/claude-1"));
        assert!(!Path::new(&wt).exists());
        assert!(git(d, &["branch", "--list", "race/*"]).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
