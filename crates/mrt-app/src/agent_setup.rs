//! `mrt agent setup | show | remove`: registers MRT's MCP server and an
//! instructions skill with coding agents (Claude Code, OpenAI Codex CLI,
//! Gemini CLI).
//!
//! Every change is planned first as a list of [`FilePlan`]s (path, contents
//! before, contents after), so `--dry-run` simply skips the apply step.
//! Existing files are merged, never replaced wholesale, and are backed up
//! before the first byte changes. Running setup twice changes nothing the
//! second time, and `remove` deletes only the `mrt` MCP entry and the
//! skill file that setup wrote.
//!
//! File locations (see docs/MCP_GUIDE.md for the sources):
//!
//! | Agent  | Scope   | MCP config                    | Skill                              |
//! |--------|---------|-------------------------------|------------------------------------|
//! | Claude | user    | `~/.claude.json`              | `~/.claude/skills/mrt/SKILL.md`    |
//! | Claude | project | `.mcp.json`                   | `.claude/skills/mrt/SKILL.md`      |
//! | Codex  | user    | `$CODEX_HOME/config.toml`     | `~/.agents/skills/mrt/SKILL.md`    |
//! | Codex  | project | `.codex/config.toml`          | `.agents/skills/mrt/SKILL.md`      |
//! | Gemini | user    | `~/.gemini/settings.json`     | `~/.gemini/skills/mrt/SKILL.md`    |
//! | Gemini | project | `.gemini/settings.json`       | `.gemini/skills/mrt/SKILL.md`      |

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Name of the MCP server entry and of the skill directory.
pub const SERVER_NAME: &str = "mrt";

/// Marker written into the skill file. `remove` and `setup` only touch a
/// skill file that contains it.
const SKILL_MARKER: &str =
    "<!-- mrt-agent-setup: managed by `mrt agent setup`; `mrt agent remove` deletes this file -->";

const SKILL_FRONTMATTER: &str = "---
name: mrt
description: Run shell commands, read their output, drive interactive programs, and inspect HTTP, DNS, ports, WebSockets and processes through the Modern Rust Terminal (MRT) MCP tools. Use when the user mentions MRT, wants commands run in their MRT terminal, or needs network or process diagnostics.
---
";

/// The instructions given to agents (the skill body, also printed by `mrt agent show`).
pub const INSTRUCTIONS: &str = "# Modern Rust Terminal (MRT)

MRT is the user's terminal. Its MCP server (tools named `mrt_*`) lets you work in it.
When an MRT window is open, the server drives that window: commands you run show up
in the user's tabs as blocks they can see. Without a window it runs headless.

## What the tools are for

- **Session and tabs**: `mrt_get_session_state` (start here: OS, shells, tabs, CWD),
  `mrt_list_tabs`, `mrt_create_tab`, `mrt_switch_tab`, `mrt_close_tab`, `mrt_configure_tab`.
- **Run commands**: `mrt_run_command` runs one command in a chosen shell (pwsh,
  powershell, cmd, bash, git-bash, wsl, zsh, fish, nu, sh) and records a block.
- **Read output instead of re-running**: `mrt_list_blocks`, `mrt_get_block`,
  `mrt_search_blocks`, `mrt_compare_blocks`; `mrt_diagnose_block` explains a failed
  command; `mrt_clear_blocks`.
- **Interactive programs** (REPLs, prompts, TUIs): `mrt_pty_spawn`, `mrt_pty_write`,
  `mrt_pty_read`, `mrt_pty_resize`, `mrt_pty_list`; always `mrt_pty_kill` when done.
- **GUI**: `mrt_control_gui` types into the user's prompt bar, toggles Mosaic, the
  Folder Explorer and the Toolbox.
- **Network diagnostics**: `mrt_http_request`, `mrt_web_fetch`, `mrt_inspect_ports`,
  `mrt_inspect_dns`, `mrt_inspect_ws`; `mrt_generate_bug_report` writes a redacted report.
- **Task Manager**: `mrt_list_processes`, `mrt_inspect_process`,
  `mrt_compare_process_snapshots`, and `mrt_kill_process` to stop a process.
- `mrt_ai_translate` turns a plain-language request into a shell command.

## Safety Approval Gate

Every command, PTY spawn and PTY input goes through MRT's Safety Approval Gate.
Commands it rates Critical (for example `rm -rf`, `Remove-Item -Recurse -Force`,
`del /s /q`, `git reset --hard`, `DROP TABLE`, killing OS processes) are refused with
`BLOCKED BY MRT SAFETY GATE`. When that happens:

- Do not retry the command in another shell, split it up or disguise it.
- Tell the user what you wanted to run and why. If they want it, send it with
  `mrt_control_gui` (`input_text` + `submit_input: true`): the MRT window then asks
  them to approve it.
- Stop processes with `mrt_kill_process`, not `kill`/`taskkill`; outside MRT's own
  processes the user must approve the request in the MRT window.
";

/// Coding agents `mrt agent` knows how to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Gemini];

    fn parse_list(s: &str) -> Result<Vec<Agent>> {
        let mut out = Vec::new();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let agents: &[Agent] = match part.to_ascii_lowercase().as_str() {
                "all" => &Agent::ALL,
                "claude" | "claude-code" => &[Agent::Claude],
                "codex" => &[Agent::Codex],
                "gemini" | "gemini-cli" => &[Agent::Gemini],
                other => bail!("unknown agent '{other}' (expected claude, codex, gemini or all)"),
            };
            for a in agents {
                if !out.contains(a) {
                    out.push(*a);
                }
            }
        }
        if out.is_empty() {
            bail!("--agent needs a value (claude, codex, gemini or all)");
        }
        Ok(out)
    }

    fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex CLI",
            Agent::Gemini => "Gemini CLI",
        }
    }
}

/// Whether to configure the agent for the current user or the current project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

/// Directories the config files are resolved against.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub project: PathBuf,
    /// `$CODEX_HOME`, `~/.codex` by default.
    pub codex_home: PathBuf,
}

impl Env {
    pub fn detect() -> Result<Self> {
        let home = directories::BaseDirs::new()
            .map(|d| d.home_dir().to_path_buf())
            .ok_or_else(|| anyhow!("could not find your home directory"))?;
        let project = std::env::current_dir().context("could not read the current directory")?;
        let codex_home = std::env::var_os("CODEX_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        Ok(Self {
            home,
            project,
            codex_home,
        })
    }
}

/// The command line agents use to start MRT's MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCommand {
    pub command: String,
    pub args: Vec<String>,
}

impl McpCommand {
    /// `<path to this mrt binary> --mcp-server`.
    pub fn current() -> Result<Self> {
        let exe = std::env::current_exe().context("could not find the path of the mrt binary")?;
        Ok(Self::new(exe.to_string_lossy().into_owned()))
    }

    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: vec!["--mcp-server".to_string()],
        }
    }

    fn shell_line(&self) -> String {
        let quote = |s: &str| {
            if s.contains(char::is_whitespace) {
                format!("\"{s}\"")
            } else {
                s.to_string()
            }
        };
        std::iter::once(quote(&self.command))
            .chain(self.args.iter().map(|a| quote(a)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFormat {
    /// `{"mcpServers": {"mrt": {"type": "stdio", "command", "args"}}}`
    ClaudeJson,
    /// `{"mcpServers": {"mrt": {"command", "args"}}}`
    GeminiJson,
    /// `[mcp_servers.mrt]` with `command` and `args`.
    CodexToml,
}

#[derive(Debug, Clone)]
struct Target {
    agent: Agent,
    mcp_config: PathBuf,
    format: ConfigFormat,
    skill_file: PathBuf,
}

fn target(agent: Agent, scope: Scope, env: &Env) -> Target {
    let skill = |base: PathBuf| base.join("skills").join(SERVER_NAME).join("SKILL.md");
    let (mcp_config, format, skill_file) = match (agent, scope) {
        (Agent::Claude, Scope::User) => (
            env.home.join(".claude.json"),
            ConfigFormat::ClaudeJson,
            skill(env.home.join(".claude")),
        ),
        (Agent::Claude, Scope::Project) => (
            env.project.join(".mcp.json"),
            ConfigFormat::ClaudeJson,
            skill(env.project.join(".claude")),
        ),
        (Agent::Codex, Scope::User) => (
            env.codex_home.join("config.toml"),
            ConfigFormat::CodexToml,
            skill(env.home.join(".agents")),
        ),
        (Agent::Codex, Scope::Project) => (
            env.project.join(".codex").join("config.toml"),
            ConfigFormat::CodexToml,
            skill(env.project.join(".agents")),
        ),
        (Agent::Gemini, Scope::User) => (
            env.home.join(".gemini").join("settings.json"),
            ConfigFormat::GeminiJson,
            skill(env.home.join(".gemini")),
        ),
        (Agent::Gemini, Scope::Project) => (
            env.project.join(".gemini").join("settings.json"),
            ConfigFormat::GeminiJson,
            skill(env.project.join(".gemini")),
        ),
    };
    Target {
        agent,
        mcp_config,
        format,
        skill_file,
    }
}

/// One planned file change. `after == None` means delete the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePlan {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: Option<String>,
    /// Why the file is left alone, when it is.
    pub skipped: Option<String>,
}

impl FilePlan {
    fn unchanged(path: &Path, before: Option<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            after: before.clone(),
            before,
            skipped: None,
        }
    }

    fn skip(path: &Path, before: Option<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            after: before.clone(),
            before,
            skipped: Some(reason.into()),
        }
    }

    pub fn is_change(&self) -> bool {
        self.skipped.is_none() && self.before != self.after
    }
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Updated { backup: Option<PathBuf> },
    Deleted { backup: Option<PathBuf> },
    Unchanged,
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct Action {
    pub agent: Agent,
    pub path: PathBuf,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub dry_run: bool,
    pub actions: Vec<Action>,
}

impl Report {
    pub fn changed(&self) -> bool {
        self.actions
            .iter()
            .any(|a| !matches!(a.outcome, Outcome::Unchanged | Outcome::Skipped(_)))
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut last_agent = None;
        for a in &self.actions {
            if last_agent != Some(a.agent) {
                writeln!(f, "{}:", a.agent.label())?;
                last_agent = Some(a.agent);
            }
            let p = a.path.display();
            let would = if self.dry_run { "would " } else { "" };
            match &a.outcome {
                Outcome::Created => writeln!(f, "  {would}create  {p}")?,
                Outcome::Updated { backup } => {
                    write!(f, "  {would}update  {p}")?;
                    match backup {
                        Some(b) => writeln!(f, " (backup: {})", b.display())?,
                        None if self.dry_run => writeln!(f, " (after a backup)")?,
                        None => writeln!(f)?,
                    }
                }
                Outcome::Deleted { backup } => {
                    write!(f, "  {would}delete  {p}")?;
                    match backup {
                        Some(b) => writeln!(f, " (backup: {})", b.display())?,
                        None => writeln!(f)?,
                    }
                }
                Outcome::Unchanged => writeln!(f, "  unchanged  {p}")?,
                Outcome::Skipped(reason) => writeln!(f, "  skipped    {p}: {reason}")?,
            }
        }
        if self.dry_run {
            writeln!(f, "\nDry run: nothing was written.")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Setup,
    Remove,
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

/// Last path component of `command` without extension, lowercased, accepting
/// both `/` and `\` separators so a Windows path is understood everywhere.
fn command_stem(command: &str) -> String {
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    };
    stem.to_ascii_lowercase()
}

/// Whether an existing server entry is one `mrt agent setup` would have written
/// (an `mrt` binary started with `--mcp-server`).
fn looks_like_mrt(command: Option<&str>, args: &[&str]) -> bool {
    command.is_some_and(|c| command_stem(c) == "mrt") && args == ["--mcp-server"]
}

// ---------------------------------------------------------------------------
// JSON configs (Claude Code, Gemini CLI)
// ---------------------------------------------------------------------------

fn json_entry_is_mrt(entry: &Value) -> bool {
    let args: Vec<&str> = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    looks_like_mrt(entry.get("command").and_then(Value::as_str), &args)
}

fn plan_json(path: &Path, format: ConfigFormat, cmd: &McpCommand, mode: Mode) -> Result<FilePlan> {
    let before = read_optional(path)?;
    let mut root: Value = match before.as_deref() {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(text).with_context(|| {
            format!("{} is not valid JSON; fix it and try again", path.display())
        })?,
        _ => Value::Object(Map::new()),
    };
    let Some(obj) = root.as_object_mut() else {
        bail!("{} does not contain a JSON object", path.display());
    };

    match mode {
        Mode::Setup => {
            let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
            let Some(servers) = servers.as_object_mut() else {
                bail!("\"mcpServers\" in {} is not an object", path.display());
            };
            if let Some(existing) = servers.get(SERVER_NAME) {
                if !json_entry_is_mrt(existing) {
                    return Ok(FilePlan::skip(
                        path,
                        before,
                        format!(
                            "an \"{SERVER_NAME}\" MCP server that is not MRT is already configured"
                        ),
                    ));
                }
            }
            let entry = servers.entry(SERVER_NAME).or_insert_with(|| json!({}));
            let Some(entry) = entry.as_object_mut() else {
                unreachable!("checked by json_entry_is_mrt");
            };
            if format == ConfigFormat::ClaudeJson {
                entry.insert("type".into(), json!("stdio"));
            }
            entry.insert("command".into(), json!(cmd.command));
            entry.insert("args".into(), json!(cmd.args));
        }
        Mode::Remove => {
            let Some(servers) = obj.get_mut("mcpServers").and_then(Value::as_object_mut) else {
                return Ok(FilePlan::unchanged(path, before));
            };
            match servers.get(SERVER_NAME) {
                None => return Ok(FilePlan::unchanged(path, before)),
                Some(e) if !json_entry_is_mrt(e) => {
                    return Ok(FilePlan::skip(
                        path,
                        before,
                        format!("the \"{SERVER_NAME}\" MCP server there was not added by mrt"),
                    ))
                }
                Some(_) => {}
            }
            servers.remove(SERVER_NAME);
            if servers.is_empty() {
                obj.remove("mcpServers");
            }
            if obj.is_empty() {
                return Ok(FilePlan {
                    path: path.to_path_buf(),
                    before,
                    after: None,
                    skipped: None,
                });
            }
        }
    }

    let mut after = serde_json::to_string_pretty(&root)?;
    after.push('\n');
    // Keep the original bytes when nothing changed semantically, so a re-run
    // never rewrites (or reformats) the file.
    if let Some(b) = &before {
        if serde_json::from_str::<Value>(b).ok().as_ref() == Some(&root) {
            return Ok(FilePlan::unchanged(path, before));
        }
    }
    Ok(FilePlan {
        path: path.to_path_buf(),
        before,
        after: Some(after),
        skipped: None,
    })
}

// ---------------------------------------------------------------------------
// TOML config (Codex CLI)
// ---------------------------------------------------------------------------

fn toml_entry_is_mrt(item: &toml_edit::Item) -> bool {
    let Some(t) = item.as_table_like() else {
        return false;
    };
    let args: Vec<&str> = t
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    looks_like_mrt(t.get("command").and_then(|c| c.as_str()), &args)
}

fn plan_toml(path: &Path, cmd: &McpCommand, mode: Mode) -> Result<FilePlan> {
    use toml_edit::{value, Array, DocumentMut, Item, Table};

    let before = read_optional(path)?;
    let mut doc: DocumentMut =
        before.as_deref().unwrap_or("").parse().with_context(|| {
            format!("{} is not valid TOML; fix it and try again", path.display())
        })?;

    match mode {
        Mode::Setup => {
            let servers = doc.as_table_mut().entry("mcp_servers").or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            });
            let Some(servers) = servers.as_table_like_mut() else {
                bail!("mcp_servers in {} is not a table", path.display());
            };
            match servers.get(SERVER_NAME) {
                Some(existing) if !toml_entry_is_mrt(existing) => {
                    return Ok(FilePlan::skip(
                        path,
                        before,
                        format!(
                            "an \"{SERVER_NAME}\" MCP server that is not MRT is already configured"
                        ),
                    ))
                }
                Some(_) => {}
                None => {
                    servers.insert(SERVER_NAME, Item::Table(Table::new()));
                }
            }
            let entry = servers
                .get_mut(SERVER_NAME)
                .and_then(Item::as_table_like_mut)
                .expect("entry inserted or checked above");
            let new_args: Array = cmd.args.iter().map(String::as_str).collect();
            let unchanged = entry.get("command").and_then(Item::as_str)
                == Some(cmd.command.as_str())
                && entry
                    .get("args")
                    .and_then(Item::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    == Some(cmd.args.iter().map(String::as_str).collect());
            if !unchanged {
                entry.insert("command", value(cmd.command.as_str()));
                entry.insert("args", value(new_args));
            }
        }
        Mode::Remove => {
            let Some(servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_like_mut) else {
                return Ok(FilePlan::unchanged(path, before));
            };
            match servers.get(SERVER_NAME) {
                None => return Ok(FilePlan::unchanged(path, before)),
                Some(e) if !toml_entry_is_mrt(e) => {
                    return Ok(FilePlan::skip(
                        path,
                        before,
                        format!("the \"{SERVER_NAME}\" MCP server there was not added by mrt"),
                    ))
                }
                Some(_) => {}
            }
            servers.remove(SERVER_NAME);
            if servers.is_empty() {
                doc.as_table_mut().remove("mcp_servers");
            }
        }
    }

    let mut after = doc.to_string();
    if after.trim().is_empty() {
        let after = if mode == Mode::Remove {
            None
        } else {
            Some(after)
        };
        return Ok(FilePlan {
            path: path.to_path_buf(),
            before,
            after,
            skipped: None,
        });
    }
    if before.as_deref() == Some(after.as_str()) {
        return Ok(FilePlan::unchanged(path, before));
    }
    if !after.ends_with('\n') {
        after.push('\n');
    }
    Ok(FilePlan {
        path: path.to_path_buf(),
        before,
        after: Some(after),
        skipped: None,
    })
}

// ---------------------------------------------------------------------------
// Skill file
// ---------------------------------------------------------------------------

/// Full contents of the `SKILL.md` that setup writes.
pub fn skill_contents() -> String {
    format!("{SKILL_FRONTMATTER}{SKILL_MARKER}\n\n{INSTRUCTIONS}")
}

fn plan_skill(path: &Path, mode: Mode) -> Result<FilePlan> {
    let before = read_optional(path)?;
    if let Some(text) = &before {
        if !text.contains(SKILL_MARKER) {
            return Ok(FilePlan::skip(
                path,
                before,
                "a skill file mrt did not write is already there",
            ));
        }
    }
    let after = match mode {
        Mode::Setup => Some(skill_contents()),
        Mode::Remove => None,
    };
    if before.is_none() && after.is_none() {
        return Ok(FilePlan::unchanged(path, None));
    }
    Ok(FilePlan {
        path: path.to_path_buf(),
        before,
        after,
        skipped: None,
    })
}

// ---------------------------------------------------------------------------
// Planning and applying
// ---------------------------------------------------------------------------

fn plan(
    agents: &[Agent],
    scope: Scope,
    env: &Env,
    cmd: &McpCommand,
    mode: Mode,
) -> Result<Vec<(Agent, FilePlan)>> {
    let mut plans = Vec::new();
    for &agent in agents {
        let t = target(agent, scope, env);
        let config = match t.format {
            ConfigFormat::CodexToml => plan_toml(&t.mcp_config, cmd, mode)?,
            f => plan_json(&t.mcp_config, f, cmd, mode)?,
        };
        plans.push((t.agent, config));
        plans.push((t.agent, plan_skill(&t.skill_file, mode)?));
    }
    Ok(plans)
}

/// Copies `path` next to itself as `<name>.mrt-backup-<timestamp>` and returns the copy.
fn backup(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut candidate = path.with_file_name(format!("{name}.mrt-backup-{stamp}"));
    let mut n = 1;
    while candidate.exists() {
        n += 1;
        candidate = path.with_file_name(format!("{name}.mrt-backup-{stamp}-{n}"));
    }
    fs::copy(path, &candidate).with_context(|| {
        format!(
            "could not back up {} to {}",
            path.display(),
            candidate.display()
        )
    })?;
    Ok(candidate)
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.mrt-tmp"));
    fs::write(&tmp, contents).with_context(|| format!("could not write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

/// Removes the folders above `path` that are now empty (the ones setup created,
/// such as `.claude/skills/mrt/`), stopping at `root` (home or project folder).
fn remove_empty_parents(path: &Path, root: &Path) {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn apply(plan: &FilePlan, is_skill: bool, root: &Path, dry_run: bool) -> Result<Outcome> {
    if let Some(reason) = &plan.skipped {
        return Ok(Outcome::Skipped(reason.clone()));
    }
    if !plan.is_change() {
        return Ok(Outcome::Unchanged);
    }
    // The skill file is entirely ours, so it needs no backup.
    let wants_backup = plan.before.is_some() && !is_skill;
    match (&plan.before, &plan.after) {
        (None, Some(after)) => {
            if !dry_run {
                write_atomic(&plan.path, after)?;
            }
            Ok(Outcome::Created)
        }
        (Some(_), Some(after)) => {
            let backup = if wants_backup && !dry_run {
                Some(backup(&plan.path)?)
            } else {
                None
            };
            if !dry_run {
                write_atomic(&plan.path, after)?;
            }
            Ok(Outcome::Updated { backup })
        }
        (Some(_), None) => {
            // A file is only deleted when nothing but MRT's own entry was in it,
            // so there is nothing of the user's to back up.
            let backup = None;
            if !dry_run {
                fs::remove_file(&plan.path)
                    .with_context(|| format!("could not delete {}", plan.path.display()))?;
                remove_empty_parents(&plan.path, root);
            }
            Ok(Outcome::Deleted { backup })
        }
        (None, None) => Ok(Outcome::Unchanged),
    }
}

fn run(
    agents: &[Agent],
    scope: Scope,
    env: &Env,
    cmd: &McpCommand,
    mode: Mode,
    dry_run: bool,
) -> Result<Report> {
    // Plan everything first so a bad config file aborts before anything is written.
    let plans = plan(agents, scope, env, cmd, mode)?;
    let root = match scope {
        Scope::User => env.home.as_path(),
        Scope::Project => env.project.as_path(),
    };
    let mut report = Report {
        dry_run,
        actions: Vec::new(),
    };
    for (i, (agent, p)) in plans.iter().enumerate() {
        let is_skill = i % 2 == 1;
        let outcome = apply(p, is_skill, root, dry_run)?;
        report.actions.push(Action {
            agent: *agent,
            path: p.path.clone(),
            outcome,
        });
    }
    Ok(report)
}

/// Registers MRT with the given agents.
pub fn setup(
    agents: &[Agent],
    scope: Scope,
    env: &Env,
    cmd: &McpCommand,
    dry_run: bool,
) -> Result<Report> {
    run(agents, scope, env, cmd, Mode::Setup, dry_run)
}

/// Undoes [`setup`]: removes the `mrt` MCP entry and the managed skill file.
pub fn remove(agents: &[Agent], scope: Scope, env: &Env, dry_run: bool) -> Result<Report> {
    // The command is irrelevant for removal; entries are recognised by shape.
    run(
        agents,
        scope,
        env,
        &McpCommand::new("mrt"),
        Mode::Remove,
        dry_run,
    )
}

/// Text printed by `mrt agent show`.
pub fn show_text(cmd: &McpCommand) -> String {
    let generic = serde_json::to_string_pretty(&json!({
        "mcpServers": { SERVER_NAME: { "command": cmd.command, "args": cmd.args } }
    }))
    .unwrap_or_default();
    format!(
        "MRT MCP server command line:\n  {}\n\n\
         For agents that read an \"mcpServers\" JSON config:\n{}\n\n\
         Instructions for the agent (installed as skills/mrt/SKILL.md by `mrt agent setup`):\n\n{}",
        cmd.shell_line(),
        generic,
        INSTRUCTIONS
    )
}

pub const USAGE: &str = "Usage:
  mrt agent setup  [--agent claude|codex|gemini|all] [--user|--project] [--dry-run]
  mrt agent show
  mrt agent remove [--agent claude|codex|gemini|all] [--user|--project] [--dry-run]

setup   registers `<this mrt> --mcp-server` in each agent's MCP config and installs
        an `mrt` skill that explains MRT's tools and its Safety Approval Gate.
        Existing files are merged and backed up first; re-running changes nothing.
show    prints the MCP command line and the instructions, for any other agent.
remove  removes exactly what setup added.

  --agent    which agents (comma-separated); default: all
  --user     configure your user-level files (default)
  --project  configure files in the current directory (.mcp.json, .codex/, .gemini/)
  --dry-run  print what would change without writing anything";

#[derive(Debug, PartialEq, Eq)]
struct Flags {
    agents: Vec<Agent>,
    scope: Scope,
    dry_run: bool,
}

fn parse_flags(args: &[String]) -> Result<Flags> {
    let mut flags = Flags {
        agents: Agent::ALL.to_vec(),
        scope: Scope::User,
        dry_run: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--agent" | "-a" => {
                let v = it.next().ok_or_else(|| anyhow!("--agent needs a value"))?;
                flags.agents = Agent::parse_list(v)?;
            }
            s if s.starts_with("--agent=") => {
                flags.agents = Agent::parse_list(&s["--agent=".len()..])?
            }
            "--user" => flags.scope = Scope::User,
            "--project" => flags.scope = Scope::Project,
            "--dry-run" | "-n" => flags.dry_run = true,
            other => bail!("unknown option '{other}'\n\n{USAGE}"),
        }
    }
    Ok(flags)
}

/// Entry point for `mrt agent ...`; `args` are the words after `agent`.
pub fn run_cli(args: &[String]) -> Result<()> {
    let Some(sub) = args.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    let rest = &args[1..];
    if rest.iter().any(|a| a == "--help" || a == "-h")
        || sub == "--help"
        || sub == "-h"
        || sub == "help"
    {
        println!("{USAGE}");
        return Ok(());
    }
    match sub.as_str() {
        "show" => {
            if let Some(extra) = rest.first() {
                bail!("unknown option '{extra}' for `mrt agent show`");
            }
            println!("{}", show_text(&McpCommand::current()?));
        }
        "setup" => {
            let f = parse_flags(rest)?;
            let env = Env::detect()?;
            let report = setup(&f.agents, f.scope, &env, &McpCommand::current()?, f.dry_run)?;
            print!("{report}");
            if !f.dry_run && report.changed() {
                println!("\nRestart the agent to load the MRT MCP server and skill.");
            }
        }
        "remove" => {
            let f = parse_flags(rest)?;
            let env = Env::detect()?;
            print!("{}", remove(&f.agents, f.scope, &env, f.dry_run)?);
        }
        other => bail!("unknown command `mrt agent {other}`\n\n{USAGE}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox {
        root: PathBuf,
        env: Env,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("mrt-agent-setup-{name}-{}", uuid::Uuid::new_v4()));
            let home = root.join("home");
            let project = root.join("project");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(&project).unwrap();
            let env = Env {
                codex_home: home.join(".codex"),
                home,
                project,
            };
            Self { root, env }
        }

        fn read(&self, path: &Path) -> String {
            fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        }

        fn write(&self, path: &Path, text: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        /// Every file under the sandbox, relative, sorted.
        fn files(&self) -> Vec<String> {
            fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
                for e in fs::read_dir(dir).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, base, out);
                    } else {
                        out.push(
                            p.strip_prefix(base)
                                .unwrap()
                                .to_string_lossy()
                                .replace('\\', "/"),
                        );
                    }
                }
            }
            let mut out = Vec::new();
            walk(&self.root, &self.root, &mut out);
            out.sort();
            out
        }

        fn backups(&self) -> Vec<String> {
            self.files()
                .into_iter()
                .filter(|f| f.contains(".mrt-backup-"))
                .collect()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn cmd() -> McpCommand {
        McpCommand::new(if cfg!(windows) {
            r"C:\Tools\MRT\mrt.exe"
        } else {
            "/opt/mrt/bin/mrt"
        })
    }

    fn json(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn fresh_user_install_writes_all_three_agents() {
        let sb = Sandbox::new("fresh");
        let report = setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), false).unwrap();
        assert!(
            report.actions.iter().all(|a| a.outcome == Outcome::Created),
            "{report}"
        );

        let claude = json(&sb.read(&sb.env.home.join(".claude.json")));
        assert_eq!(claude["mcpServers"]["mrt"]["type"], "stdio");
        assert_eq!(claude["mcpServers"]["mrt"]["command"], cmd().command);
        assert_eq!(claude["mcpServers"]["mrt"]["args"], json!(["--mcp-server"]));

        let gemini = json(&sb.read(&sb.env.home.join(".gemini/settings.json")));
        assert_eq!(gemini["mcpServers"]["mrt"]["command"], cmd().command);
        assert!(gemini["mcpServers"]["mrt"].get("type").is_none());

        let codex: toml_edit::DocumentMut = sb
            .read(&sb.env.codex_home.join("config.toml"))
            .parse()
            .unwrap();
        assert_eq!(
            codex["mcp_servers"]["mrt"]["command"].as_str(),
            Some(cmd().command.as_str())
        );
        assert_eq!(
            codex["mcp_servers"]["mrt"]["args"]
                .as_array()
                .unwrap()
                .get(0)
                .and_then(|v| v.as_str()),
            Some("--mcp-server")
        );

        for skill in [
            ".claude/skills/mrt/SKILL.md",
            ".agents/skills/mrt/SKILL.md",
            ".gemini/skills/mrt/SKILL.md",
        ] {
            let text = sb.read(&sb.env.home.join(skill));
            assert!(text.starts_with("---\nname: mrt\ndescription: "), "{skill}");
            assert!(text.contains("Safety Approval Gate"));
        }
        assert!(sb.backups().is_empty());
    }

    #[test]
    fn project_scope_uses_project_files() {
        let sb = Sandbox::new("project");
        setup(&Agent::ALL, Scope::Project, &sb.env, &cmd(), false).unwrap();
        let files = sb.files();
        for f in [
            "project/.mcp.json",
            "project/.claude/skills/mrt/SKILL.md",
            "project/.codex/config.toml",
            "project/.agents/skills/mrt/SKILL.md",
            "project/.gemini/settings.json",
            "project/.gemini/skills/mrt/SKILL.md",
        ] {
            assert!(files.contains(&f.to_string()), "missing {f} in {files:?}");
        }
        assert!(!files.iter().any(|f| f.starts_with("home/")));

        remove(&Agent::ALL, Scope::Project, &sb.env, false).unwrap();
        let left: Vec<_> = fs::read_dir(&sb.env.project)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert!(left.is_empty(), "remove left folders behind: {left:?}");
        assert!(sb.env.project.exists(), "the project folder itself stays");
    }

    #[test]
    fn merges_into_existing_configs_and_backs_them_up() {
        let sb = Sandbox::new("merge");
        let claude_path = sb.env.home.join(".claude.json");
        let claude_before = r#"{"numStartups": 7, "mcpServers": {"git": {"command": "uvx", "args": ["mcp-server-git"]}}, "projects": {"/x": {"mcpServers": {}}}}"#;
        sb.write(&claude_path, claude_before);
        let gemini_path = sb.env.home.join(".gemini/settings.json");
        sb.write(&gemini_path, r#"{"theme": "GitHub"}"#);
        let codex_path = sb.env.codex_home.join("config.toml");
        let codex_before = "# my settings\nmodel = \"o4-mini\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs-mcp\"]\n";
        sb.write(&codex_path, codex_before);

        let report = setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), false).unwrap();
        let updated = report
            .actions
            .iter()
            .filter(|a| matches!(a.outcome, Outcome::Updated { .. }))
            .count();
        assert_eq!(updated, 3, "{report}");

        let claude = json(&sb.read(&claude_path));
        assert_eq!(claude["numStartups"], 7);
        assert_eq!(claude["mcpServers"]["git"]["command"], "uvx");
        assert_eq!(claude["projects"], json!({"/x": {"mcpServers": {}}}));
        assert_eq!(claude["mcpServers"]["mrt"]["command"], cmd().command);

        let gemini = json(&sb.read(&gemini_path));
        assert_eq!(gemini["theme"], "GitHub");
        assert_eq!(gemini["mcpServers"]["mrt"]["args"], json!(["--mcp-server"]));

        let codex = sb.read(&codex_path);
        assert!(
            codex.starts_with(codex_before),
            "existing TOML kept verbatim:\n{codex}"
        );
        assert!(codex.contains("[mcp_servers.mrt]"));

        // One backup per pre-existing config, holding the original bytes.
        let backups = sb.backups();
        assert_eq!(backups.len(), 3, "{backups:?}");
        let claude_backup = backups
            .iter()
            .find(|b| b.starts_with("home/.claude.json.mrt-backup-"))
            .unwrap();
        assert_eq!(sb.read(&sb.root.join(claude_backup)), claude_before);
    }

    #[test]
    fn rerun_is_idempotent() {
        let sb = Sandbox::new("idempotent");
        sb.write(
            &sb.env.home.join(".gemini/settings.json"),
            "{\n  \"theme\": \"GitHub\"\n}\n",
        );
        setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), false).unwrap();
        let snapshot: Vec<(String, String)> = sb
            .files()
            .into_iter()
            .map(|f| (f.clone(), sb.read(&sb.root.join(&f))))
            .collect();

        let second = setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), false).unwrap();
        assert!(
            second
                .actions
                .iter()
                .all(|a| a.outcome == Outcome::Unchanged),
            "{second}"
        );
        assert!(!second.changed());
        let after: Vec<(String, String)> = sb
            .files()
            .into_iter()
            .map(|f| (f.clone(), sb.read(&sb.root.join(&f))))
            .collect();
        assert_eq!(snapshot, after);
    }

    #[test]
    fn moved_binary_updates_entry_but_keeps_user_tweaks() {
        let sb = Sandbox::new("moved");
        setup(
            &[Agent::Claude, Agent::Codex],
            Scope::User,
            &sb.env,
            &McpCommand::new("/old/mrt"),
            false,
        )
        .unwrap();
        // The user adds env vars to the entry by hand.
        let claude_path = sb.env.home.join(".claude.json");
        let mut v = json(&sb.read(&claude_path));
        v["mcpServers"]["mrt"]["env"] = json!({"RUST_LOG": "debug"});
        sb.write(&claude_path, &serde_json::to_string(&v).unwrap());

        setup(
            &[Agent::Claude, Agent::Codex],
            Scope::User,
            &sb.env,
            &cmd(),
            false,
        )
        .unwrap();
        let v = json(&sb.read(&claude_path));
        assert_eq!(v["mcpServers"]["mrt"]["command"], cmd().command);
        assert_eq!(v["mcpServers"]["mrt"]["env"]["RUST_LOG"], "debug");
        let codex = sb.read(&sb.env.codex_home.join("config.toml"));
        assert!(!codex.contains("/old/mrt"), "{codex}");
        assert_eq!(codex.matches("[mcp_servers.mrt]").count(), 1);
    }

    #[test]
    fn does_not_clobber_foreign_entries_or_skills() {
        let sb = Sandbox::new("foreign");
        let claude_path = sb.env.home.join(".claude.json");
        let foreign = r#"{"mcpServers": {"mrt": {"command": "node", "args": ["mars-rover.js"]}}}"#;
        sb.write(&claude_path, foreign);
        let skill = sb.env.home.join(".claude/skills/mrt/SKILL.md");
        sb.write(&skill, "---\nname: mrt\n---\nSomeone else's skill\n");

        let report = setup(&[Agent::Claude], Scope::User, &sb.env, &cmd(), false).unwrap();
        assert!(
            report
                .actions
                .iter()
                .all(|a| matches!(a.outcome, Outcome::Skipped(_))),
            "{report}"
        );
        assert_eq!(sb.read(&claude_path), foreign);
        assert!(sb.read(&skill).contains("Someone else's skill"));

        let report = remove(&[Agent::Claude], Scope::User, &sb.env, false).unwrap();
        assert!(
            report
                .actions
                .iter()
                .all(|a| matches!(a.outcome, Outcome::Skipped(_))),
            "{report}"
        );
        assert_eq!(sb.read(&claude_path), foreign);
        assert!(skill.exists());
    }

    #[test]
    fn remove_undoes_setup_on_fresh_install() {
        let sb = Sandbox::new("remove-fresh");
        setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), false).unwrap();
        let report = remove(&Agent::ALL, Scope::User, &sb.env, false).unwrap();
        assert!(
            report
                .actions
                .iter()
                .all(|a| matches!(a.outcome, Outcome::Deleted { .. })),
            "{report}"
        );
        assert!(sb.files().is_empty(), "left behind: {:?}", sb.files());
        let left: Vec<_> = fs::read_dir(&sb.env.home)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert!(left.is_empty(), "remove left folders behind: {left:?}");
        // Removing again is a no-op.
        let again = remove(&Agent::ALL, Scope::User, &sb.env, false).unwrap();
        assert!(
            again
                .actions
                .iter()
                .all(|a| a.outcome == Outcome::Unchanged),
            "{again}"
        );
    }

    #[test]
    fn remove_restores_merged_configs() {
        let sb = Sandbox::new("remove-merged");
        let claude_path = sb.env.home.join(".claude.json");
        sb.write(
            &claude_path,
            r#"{"numStartups": 7, "mcpServers": {"git": {"command": "uvx"}}}"#,
        );
        let gemini_path = sb.env.home.join(".gemini/settings.json");
        sb.write(&gemini_path, r#"{"theme": "GitHub"}"#);
        let codex_path = sb.env.codex_home.join("config.toml");
        let codex_before = "model = \"o4-mini\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\n";
        sb.write(&codex_path, codex_before);

        setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), false).unwrap();
        remove(&Agent::ALL, Scope::User, &sb.env, false).unwrap();

        assert_eq!(
            json(&sb.read(&claude_path)),
            json!({"numStartups": 7, "mcpServers": {"git": {"command": "uvx"}}})
        );
        assert_eq!(json(&sb.read(&gemini_path)), json!({"theme": "GitHub"}));
        assert_eq!(sb.read(&codex_path), codex_before);
        assert!(!sb.env.home.join(".claude/skills").exists());
        assert!(!sb.env.home.join(".agents").join("skills").exists());
    }

    #[test]
    fn dry_run_writes_nothing() {
        let sb = Sandbox::new("dry-run");
        let gemini_path = sb.env.home.join(".gemini/settings.json");
        sb.write(&gemini_path, r#"{"theme": "GitHub"}"#);
        let before = sb.files();

        let report = setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), true).unwrap();
        assert!(report.changed());
        assert!(report.to_string().contains("would create"));
        assert!(report.to_string().contains("would update"));
        assert_eq!(sb.files(), before);
        assert_eq!(sb.read(&gemini_path), r#"{"theme": "GitHub"}"#);

        setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), false).unwrap();
        let installed = sb.files();
        let report = remove(&Agent::ALL, Scope::User, &sb.env, true).unwrap();
        assert!(report.to_string().contains("would delete"));
        assert_eq!(sb.files(), installed);
    }

    #[test]
    fn invalid_config_aborts_before_writing() {
        let sb = Sandbox::new("invalid");
        sb.write(&sb.env.home.join(".gemini/settings.json"), "{ not json");
        let err = setup(&Agent::ALL, Scope::User, &sb.env, &cmd(), false).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "{err}");
        assert_eq!(sb.files(), vec!["home/.gemini/settings.json".to_string()]);
    }

    #[test]
    fn parses_flags() {
        let args = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(
            parse_flags(&args("")).unwrap(),
            Flags {
                agents: Agent::ALL.to_vec(),
                scope: Scope::User,
                dry_run: false
            }
        );
        assert_eq!(
            parse_flags(&args("--agent codex --project --dry-run")).unwrap(),
            Flags {
                agents: vec![Agent::Codex],
                scope: Scope::Project,
                dry_run: true
            }
        );
        assert_eq!(
            parse_flags(&args("--agent=claude,gemini")).unwrap().agents,
            vec![Agent::Claude, Agent::Gemini]
        );
        assert!(parse_flags(&args("--agent cursor")).is_err());
        assert!(parse_flags(&args("--bogus")).is_err());
    }

    #[test]
    fn recognises_mrt_commands() {
        assert_eq!(
            command_stem(r"C:\Users\me\MRT\target\release\mrt.exe"),
            "mrt"
        );
        assert_eq!(command_stem("/usr/local/bin/mrt"), "mrt");
        assert!(looks_like_mrt(Some("MRT.EXE"), &["--mcp-server"]));
        assert!(!looks_like_mrt(Some("/usr/bin/node"), &["--mcp-server"]));
        assert!(!looks_like_mrt(Some("mrt"), &[]));
    }

    #[test]
    fn show_text_has_command_and_instructions() {
        let text = show_text(&McpCommand::new("/Applications/My Apps/mrt"));
        assert!(text.contains("\"/Applications/My Apps/mrt\" --mcp-server"));
        assert!(text.contains("\"mcpServers\""));
        assert!(text.contains("Safety Approval Gate"));
    }
}
