//! Git worktrees for worktree tabs (`:worktree <branch>`, `mrt_create_tab` with
//! `worktree`): one checkout per branch next to the repository, so agents working
//! in parallel never touch each other's files.
//!
//! Everything goes through the `git` command line (no libgit2), started without a
//! console window on Windows.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A worktree a tab works in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// The main repository's top folder.
    pub repo: PathBuf,
    /// The worktree's folder.
    pub path: PathBuf,
    /// The branch checked out in it.
    pub branch: String,
}

/// One entry of `git worktree list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    /// `None` for a detached HEAD.
    pub branch: Option<String>,
    /// The main checkout (the first entry).
    pub main: bool,
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs git and returns its trimmed stdout, or its stderr as the error.
fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(dir)
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("git {} failed", args.join(" ")) } else { err })
    }
}

fn clean_path(path: PathBuf) -> PathBuf {
    // `std::fs::canonicalize` gives `\\?\C:\…` on Windows; git and shells want `C:\…`.
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path,
    }
}

/// The main repository's top folder for `dir` (also when `dir` is inside a worktree).
pub fn main_repo_root(dir: &Path) -> Result<PathBuf, String> {
    let common = run(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map_err(|_| format!("{} is not inside a git repository", dir.display()))?;
    let common = PathBuf::from(common);
    // The common dir is `<repo>/.git` for normal repositories.
    let root = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf).unwrap_or(common)
    } else {
        // Bare repository: no main checkout, use the folder itself.
        common
    };
    Ok(clean_path(std::fs::canonicalize(&root).unwrap_or(root)))
}

/// Checks a branch name the way git does (`git check-ref-format --branch`).
pub fn validate_branch(repo: &Path, branch: &str) -> Result<(), String> {
    if branch.trim().is_empty() || branch.starts_with('-') {
        return Err(format!("'{branch}' is not a valid branch name"));
    }
    run(repo, &["check-ref-format", "--branch", branch])
        .map(|_| ())
        .map_err(|_| format!("'{branch}' is not a valid branch name"))
}

/// Where the worktree for `branch` goes: `<parent>/<repo>.worktrees/<branch>`, with
/// the slashes of `feature/x` turned into `-` so each branch is one folder.
pub fn default_path(repo: &Path, branch: &str) -> PathBuf {
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    let folder: String = branch
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' { '-' } else { c })
        .collect();
    repo.parent().unwrap_or(repo).join(format!("{name}.worktrees")).join(folder)
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    run(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok()
}

/// Creates (or reuses) the worktree for `branch` of the repository that contains
/// `dir`. A new branch starts from `base` (default: the current `HEAD`); an existing
/// branch is checked out as it is. When the branch already has a worktree, that one
/// is returned instead of creating another.
pub fn create(dir: &Path, branch: &str, base: Option<&str>) -> Result<Worktree, String> {
    let repo = main_repo_root(dir)?;
    validate_branch(&repo, branch)?;
    if let Some(existing) = list(&repo)?.into_iter().find(|e| e.branch.as_deref() == Some(branch)) {
        return Ok(Worktree { repo, path: existing.path, branch: branch.to_string() });
    }
    let path = default_path(&repo, branch);
    if path.exists() {
        return Err(format!("{} already exists and is not a worktree of {branch}", path.display()));
    }
    let path_text = path.to_string_lossy().into_owned();
    if branch_exists(&repo, branch) {
        if base.is_some() {
            return Err(format!("branch {branch} already exists; leave out the base to check it out"));
        }
        run(&repo, &["worktree", "add", &path_text, branch])?;
    } else {
        let mut args = vec!["worktree", "add", "-b", branch, &path_text];
        if let Some(base) = base {
            args.push(base);
        }
        run(&repo, &args)?;
    }
    let path = clean_path(std::fs::canonicalize(&path).unwrap_or(path));
    Ok(Worktree { repo, path, branch: branch.to_string() })
}

/// The worktrees of the repository that contains `dir`, main checkout first.
pub fn list(dir: &Path) -> Result<Vec<WorktreeEntry>, String> {
    let text = run(dir, &["worktree", "list", "--porcelain"])?;
    Ok(parse_list(&text))
}

fn parse_list(text: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    for record in text.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        let mut bare = false;
        for line in record.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            } else if line == "bare" {
                bare = true;
            }
        }
        if let Some(path) = path {
            if !bare {
                let main = entries.is_empty();
                entries.push(WorktreeEntry { path: clean_path(path), branch, main });
            }
        }
    }
    entries
}

/// Files with uncommitted changes (including untracked files) in a worktree.
pub fn changed_files(path: &Path) -> Result<Vec<String>, String> {
    let text = run(path, &["status", "--porcelain"])?;
    Ok(text.lines().filter(|l| l.len() > 3).map(|l| l[3..].to_string()).collect())
}

/// Removes a worktree folder (`git worktree remove`, which refuses when it has
/// uncommitted changes). The branch and its commits stay in the repository.
pub fn remove(worktree: &Worktree) -> Result<(), String> {
    let path = worktree.path.to_string_lossy().into_owned();
    run(&worktree.repo, &["worktree", "remove", &path])?;
    let _ = run(&worktree.repo, &["worktree", "prune"]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    fn temp_repo() -> PathBuf {
        let base = std::env::temp_dir().join(format!("mrt-worktree-test-{}", uuid::Uuid::new_v4().simple()));
        let repo = base.join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "test@example.invalid"],
            vec!["config", "user.name", "MRT test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            run(&repo, &args).unwrap();
        }
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        run(&repo, &["add", "a.txt"]).unwrap();
        run(&repo, &["commit", "-q", "-m", "first"]).unwrap();
        clean_path(std::fs::canonicalize(&repo).unwrap())
    }

    #[test]
    fn worktree_paths_sit_next_to_the_repository() {
        let path = default_path(Path::new("/src/proj"), "feature/login");
        assert_eq!(path, Path::new("/src/proj.worktrees/feature-login"));
    }

    #[test]
    fn porcelain_list_is_parsed() {
        let text = "worktree /src/proj\nHEAD abc\nbranch refs/heads/main\n\nworktree /src/proj.worktrees/x\nHEAD def\nbranch refs/heads/feature/x\n\nworktree /src/proj.worktrees/d\nHEAD 123\ndetached\n";
        let entries = parse_list(text);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].main);
        assert_eq!(entries[1].branch.as_deref(), Some("feature/x"));
        assert_eq!(entries[2].branch, None);
        assert!(!entries[2].main);
    }

    #[test]
    fn worktrees_are_created_reused_and_removed() {
        if !git_available() {
            return;
        }
        let repo = temp_repo();
        let base = repo.parent().unwrap().to_path_buf();

        let wt = create(&repo, "feature/one", None).unwrap();
        assert_eq!(wt.repo, repo);
        assert_eq!(wt.branch, "feature/one");
        assert!(wt.path.join("a.txt").exists(), "checked out at {}", wt.path.display());
        assert!(wt.path.ends_with("proj.worktrees/feature-one") || wt.path.ends_with(r"proj.worktrees\feature-one"));

        // From inside the worktree, the main repository is still found, and asking
        // again for the same branch returns the same worktree.
        assert_eq!(main_repo_root(&wt.path).unwrap(), repo);
        assert_eq!(create(&wt.path, "feature/one", None).unwrap().path, wt.path);
        let entries = list(&repo).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.branch.as_deref() == Some("feature/one")));

        // Uncommitted changes block removal.
        std::fs::write(wt.path.join("b.txt"), "b").unwrap();
        assert_eq!(changed_files(&wt.path).unwrap(), vec!["b.txt".to_string()]);
        assert!(remove(&wt).is_err());
        std::fs::remove_file(wt.path.join("b.txt")).unwrap();
        assert!(changed_files(&wt.path).unwrap().is_empty());
        remove(&wt).unwrap();
        assert!(!wt.path.exists());
        // The branch survives the worktree.
        assert!(branch_exists(&repo, "feature/one"));
        // An existing branch is checked out again; a base is refused for it.
        assert!(create(&repo, "feature/one", Some("main")).is_err());
        let again = create(&repo, "feature/one", None).unwrap();
        assert!(again.path.exists());
        remove(&again).unwrap();

        assert!(create(&repo, "bad..name", None).is_err());
        assert!(create(&repo, "-x", None).is_err());
        assert!(main_repo_root(&base).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
