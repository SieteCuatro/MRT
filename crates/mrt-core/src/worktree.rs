//! Git worktrees for worktree tabs (`:worktree <branch>`, `mrt_create_tab` with
//! `worktree`): one checkout per branch next to the repository, so agents working
//! in parallel never touch each other's files.
//!
//! Everything goes through the `git` command line (no libgit2), started without a
//! console window on Windows.

use crate::pty::ShellKind;
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
    /// The branch it was created from, which its changes are compared with and merged
    /// into (`None` in layouts saved before it was recorded: see [`base_branch`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
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

/// The top folder of the checkout that contains `dir` (a worktree's own folder for a
/// worktree, unlike [`main_repo_root`]), or `None` outside a git repository.
pub fn checkout_root(dir: &Path) -> Option<PathBuf> {
    let top = run(dir, &["rev-parse", "--show-toplevel"]).ok().filter(|t| !t.is_empty())?;
    let top = PathBuf::from(top);
    Some(clean_path(std::fs::canonicalize(&top).unwrap_or(top)))
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
    // What the changes are compared with later: the base given, else the branch
    // checked out where the command ran (when it is not the worktree's own branch).
    let recorded_base = base
        .map(str::to_string)
        .or_else(|| current_branch(dir))
        .filter(|b| b != branch);
    if let Some(existing) = list(&repo)?.into_iter().find(|e| e.branch.as_deref() == Some(branch)) {
        return Ok(Worktree { repo, path: existing.path, branch: branch.to_string(), base: recorded_base });
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
    Ok(Worktree { repo, path, branch: branch.to_string(), base: recorded_base })
}

/// The branch checked out in `dir`, `None` when HEAD is detached.
pub fn current_branch(dir: &Path) -> Option<String> {
    run(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok().filter(|b| !b.is_empty())
}

/// The branch a worktree's changes are compared with and merged into: the one it was
/// created from, else the remote's default branch, else `main` or `master`.
pub fn base_branch(worktree: &Worktree) -> Result<String, String> {
    if let Some(base) = worktree.base.as_ref().filter(|b| branch_exists(&worktree.repo, b)) {
        return Ok(base.clone());
    }
    if let Ok(remote_head) = run(&worktree.repo, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some(b) = remote_head.strip_prefix("origin/").filter(|b| *b != worktree.branch && branch_exists(&worktree.repo, b)) {
            return Ok(b.to_string());
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|b| *b != worktree.branch && branch_exists(&worktree.repo, b))
        .map(str::to_string)
        .ok_or_else(|| format!("no base branch for {}: none was recorded and the repository has no main or master branch", worktree.branch))
}

/// What a worktree changed compared with its base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub base: String,
    /// Commits on the branch that the base does not have.
    pub ahead: usize,
    /// Commits on the base that the branch does not have.
    pub behind: usize,
    /// `git log --oneline` of the commits ahead (newest first, at most 50).
    pub commits: Vec<String>,
    /// `git diff --stat` against the point where the branch left the base.
    pub stat: String,
    /// Files with uncommitted changes in the worktree (not part of the diff).
    pub uncommitted: Vec<String>,
    /// The diff of the committed changes, with ANSI colors.
    pub diff: String,
    /// The diff was cut at [`MAX_REVIEW_DIFF`] bytes.
    pub diff_truncated: bool,
}

/// Longest diff a review block shows.
pub const MAX_REVIEW_DIFF: usize = 256 * 1024;

/// Compares a worktree's branch with its base branch.
pub fn review(worktree: &Worktree) -> Result<Review, String> {
    let base = base_branch(worktree)?;
    let dir = &worktree.path;
    let range = format!("{base}...{}", worktree.branch);
    let counts = run(dir, &["rev-list", "--left-right", "--count", &range])?;
    let mut parts = counts.split_whitespace().map(|n| n.parse::<usize>().unwrap_or(0));
    let (behind, ahead) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    let log = run(dir, &["log", "--oneline", "--no-decorate", "-n", "50", &format!("{base}..{}", worktree.branch)])?;
    let stat = run(dir, &["diff", "--stat", &range])?;
    let mut diff = run(dir, &["-c", "color.ui=always", "diff", &range])?;
    let diff_truncated = diff.len() > MAX_REVIEW_DIFF;
    if diff_truncated {
        let mut cut = MAX_REVIEW_DIFF;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
    }
    Ok(Review {
        base,
        ahead,
        behind,
        commits: log.lines().map(str::to_string).collect(),
        stat,
        uncommitted: changed_files(dir)?,
        diff,
        diff_truncated,
    })
}

impl Review {
    /// The text of a review block.
    pub fn render(&self, worktree: &Worktree) -> String {
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let mut out = format!(
            "{} → {}: {} ahead, {} behind\n",
            worktree.branch,
            self.base,
            plural(self.ahead, "commit"),
            plural(self.behind, "commit")
        );
        if !self.uncommitted.is_empty() {
            out.push_str(&format!(
                "\n{} not committed yet (not in the diff, and not merged):\n",
                plural(self.uncommitted.len(), "file")
            ));
            for file in &self.uncommitted {
                out.push_str(&format!("  {file}\n"));
            }
        }
        if self.ahead == 0 {
            out.push_str(&format!("\nNo commits to merge: {} has nothing {} does not have.\n", worktree.branch, self.base));
            return out;
        }
        out.push_str("\nCommits:\n");
        for commit in &self.commits {
            out.push_str(&format!("  {commit}\n"));
        }
        out.push_str(&format!("\nFiles:\n{}\n\n", self.stat));
        out.push_str(&self.diff);
        if self.diff_truncated {
            out.push_str(&format!("\n… diff cut at {} KiB; run `git diff {}...{}` for all of it.\n", MAX_REVIEW_DIFF / 1024, self.base, worktree.branch));
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\nNext: :worktree merge · :worktree pr · :worktree discard\n");
        out
    }
}

/// A path as the shell expects it, in double quotes (WSL gets its `/mnt/<drive>` form).
fn shell_path(path: &Path, shell: &ShellKind) -> String {
    let text = path.to_string_lossy().into_owned();
    let text = match shell {
        ShellKind::Wsl => windows_to_wsl_path(&text),
        _ => text,
    };
    format!("\"{text}\"")
}

fn windows_to_wsl_path(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let rest = path[2..].replace('\\', "/");
        format!("/mnt/{}{}", (bytes[0] as char).to_ascii_lowercase(), rest)
    } else {
        path.replace('\\', "/")
    }
}

/// The command that merges the worktree's branch into its base, run in the main
/// checkout (`git -C <repo> merge --no-ff --no-edit <branch>`). The main checkout must have the
/// base checked out; see [`merge_blocker`].
pub fn merge_command(worktree: &Worktree, shell: &ShellKind) -> String {
    format!("git -C {} merge --no-ff --no-edit {}", shell_path(&worktree.repo, shell), worktree.branch)
}

/// Why the branch cannot be merged from MRT right now, if anything stops it.
pub fn merge_blocker(worktree: &Worktree, base: &str) -> Option<String> {
    match current_branch(&worktree.repo) {
        Some(b) if b == base => None,
        Some(b) => Some(format!(
            "The main checkout ({}) has {b} checked out; check out {base} there first, then merge again.",
            worktree.repo.display()
        )),
        None => Some(format!("The main checkout ({}) has no branch checked out; check out {base} there first.", worktree.repo.display())),
    }
}

/// The command that pushes the branch to `origin` and opens a pull request into the
/// base with GitHub's `gh` (title and body from the commits), in the syntax of `shell`.
pub fn pr_command(worktree: &Worktree, base: &str, shell: &ShellKind) -> String {
    let p = shell_path(&worktree.path, shell);
    let b = &worktree.branch;
    let push = format!("git -C {p} push -u origin {b}");
    let pr = format!("gh pr create --base {base} --head {b} --fill");
    match shell {
        ShellKind::WindowsPowerShell => format!("{push}; if ($LASTEXITCODE -eq 0) {{ Set-Location {p}; {pr} }}"),
        ShellKind::PowerShellCore => format!("{push} && Set-Location {p} && {pr}"),
        ShellKind::CmdBatch => format!("{push} && cd /d {p} && {pr}"),
        ShellKind::Nushell => format!("{push}; cd {p}; {pr}"),
        _ => format!("{push} && cd {p} && {pr}"),
    }
}

/// Why a pull request cannot be opened from MRT, if anything stops it (no `origin`
/// remote, or `gh` not installed when the tab's shell runs on this machine).
pub fn pr_blocker(worktree: &Worktree, shell: &ShellKind) -> Option<String> {
    if run(&worktree.repo, &["remote", "get-url", "origin"]).is_err() {
        return Some("The repository has no `origin` remote to push the branch to.".into());
    }
    if !matches!(shell, ShellKind::Wsl) {
        let mut gh = Command::new("gh");
        gh.arg("--version");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            gh.creation_flags(0x08000000);
        }
        if !gh.output().is_ok_and(|o| o.status.success()) {
            return Some("GitHub's `gh` command is not installed (https://cli.github.com); push the branch and open the pull request by hand.".into());
        }
    }
    None
}

/// The commands [`discard`] runs, as shown in the Safety Gate.
pub fn discard_command(worktree: &Worktree) -> String {
    format!(
        "git -C \"{}\" worktree remove --force \"{}\"\ngit -C \"{}\" branch -D {}",
        worktree.repo.display(),
        worktree.path.display(),
        worktree.repo.display(),
        worktree.branch
    )
}

/// Throws the worktree away: removes its folder with any uncommitted changes and
/// deletes its branch with any commits not merged elsewhere. Retries the removal for
/// a moment, since on Windows a shell that just ended can still hold the folder.
pub fn discard(worktree: &Worktree) -> Result<(), String> {
    let path = worktree.path.to_string_lossy().into_owned();
    let mut last_error = String::new();
    for attempt in 0..10 {
        match run(&worktree.repo, &["worktree", "remove", "--force", &path]) {
            Ok(_) => {
                last_error.clear();
                break;
            }
            Err(e) => {
                last_error = e;
                if !worktree.path.exists() {
                    let _ = run(&worktree.repo, &["worktree", "prune"]);
                    last_error.clear();
                    break;
                }
                if attempt < 9 {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }
    if !last_error.is_empty() {
        return Err(last_error);
    }
    run(&worktree.repo, &["branch", "-D", &worktree.branch]).map(|_| ())
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
    fn merge_and_pr_commands_follow_the_shell() {
        let wt = Worktree {
            repo: PathBuf::from(r"C:\src\proj"),
            path: PathBuf::from(r"C:\src\proj.worktrees\feat-x"),
            branch: "feat/x".into(),
            base: Some("main".into()),
        };
        assert_eq!(merge_command(&wt, &ShellKind::PowerShellCore), r#"git -C "C:\src\proj" merge --no-ff --no-edit feat/x"#);
        assert_eq!(merge_command(&wt, &ShellKind::Wsl), r#"git -C "/mnt/c/src/proj" merge --no-ff --no-edit feat/x"#);
        let push = r#"git -C "C:\src\proj.worktrees\feat-x" push -u origin feat/x"#;
        let pr = "gh pr create --base main --head feat/x --fill";
        assert_eq!(
            pr_command(&wt, "main", &ShellKind::WindowsPowerShell),
            format!(r#"{push}; if ($LASTEXITCODE -eq 0) {{ Set-Location "C:\src\proj.worktrees\feat-x"; {pr} }}"#)
        );
        assert_eq!(pr_command(&wt, "main", &ShellKind::CmdBatch), format!(r#"{push} && cd /d "C:\src\proj.worktrees\feat-x" && {pr}"#));
        assert_eq!(
            pr_command(&wt, "main", &ShellKind::Bash),
            format!(r#"{push} && cd "C:\src\proj.worktrees\feat-x" && {pr}"#)
        );
        assert!(pr_command(&wt, "main", &ShellKind::Wsl).starts_with(r#"git -C "/mnt/c/src/proj.worktrees/feat-x" push"#));
        assert!(discard_command(&wt).contains("branch -D feat/x"));
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

        // Review, merge and discard.
        let wt = create(&repo, "feature/two", None).unwrap();
        assert_eq!(wt.base.as_deref(), Some("main"), "recorded from the checkout it was made in");
        let empty = review(&wt).unwrap();
        assert_eq!((empty.ahead, empty.behind), (0, 0));
        assert!(empty.render(&wt).contains("No commits to merge"));
        std::fs::write(wt.path.join("b.txt"), "b").unwrap();
        run(&wt.path, &["add", "b.txt"]).unwrap();
        run(&wt.path, &["commit", "-q", "-m", "add b"]).unwrap();
        std::fs::write(wt.path.join("c.txt"), "c").unwrap();
        let r = review(&wt).unwrap();
        assert_eq!((r.ahead, r.behind, r.base.as_str()), (1, 0, "main"));
        assert!(r.commits[0].ends_with("add b"));
        assert!(r.stat.contains("b.txt"));
        assert_eq!(r.uncommitted, vec!["c.txt".to_string()]);
        let text = r.render(&wt);
        assert!(text.contains("feature/two → main: 1 commit ahead, 0 commits behind"), "{text}");
        assert!(text.contains("c.txt") && r.diff.contains("b.txt") && r.diff.contains('\u{1b}'), "colored diff: {text}");
        assert_eq!(merge_blocker(&wt, "main"), None);
        assert!(merge_blocker(&wt, "develop").is_some());
        assert!(pr_blocker(&wt, &ShellKind::Bash).unwrap().contains("origin"));
        // Run the merge command the way a POSIX shell would.
        let merged = run(&repo, &["merge", "--no-ff", "-q", "-m", "merge", "feature/two"]);
        assert!(merged.is_ok(), "{merged:?}");
        assert!(repo.join("b.txt").exists());
        assert_eq!(review(&wt).unwrap().ahead, 0);
        discard(&wt).unwrap();
        assert!(!wt.path.exists());
        assert!(!branch_exists(&repo, "feature/two"));
        assert!(create(&repo, "-x", None).is_err());
        assert!(main_repo_root(&base).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
