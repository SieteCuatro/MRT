//! `mrt list | send | capture | split`: command-line control of a running MRT window.
//!
//! Each subcommand is a thin wrapper around one MCP tool, sent over the
//! token-authenticated localhost bridge the GUI opens (`mcp-bridge.json`), so the
//! CLI sees exactly what agents see. Targets (`-t`) take a tab index, UUID, or
//! title substring, like the MCP tools.

use serde_json::{json, Value};

/// Exit code when the arguments are wrong.
pub const EXIT_USAGE: i32 = 2;
/// Exit code when no MRT window is running (or its bridge refused the token).
pub const EXIT_NO_WINDOW: i32 = 69;
/// Exit code when MRT reported an error (unknown tab, refused command...).
pub const EXIT_TOOL_ERROR: i32 = 70;

#[derive(Debug, Clone, PartialEq)]
pub enum CliCommand {
    List,
    Send { command: String, shell: Option<String>, cwd: Option<String>, idle: bool },
    Capture { lines: Option<usize>, blocks: usize },
    Split { direction: SplitArg },
    /// `mrt quick [show|hide|toggle]`: the quick terminal.
    Quick { action: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SplitArg {
    Horizontal,
    Vertical,
    Off,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliInvocation {
    pub command: CliCommand,
    pub target: Option<String>,
    pub json: bool,
}

pub const SUBCOMMANDS: &[&str] = &["list", "send", "capture", "split", "quick", "quit"];

pub const USAGE: &str = "\
Control a running MRT window:
  mrt list [--json]                       List tabs (index, title, shell, folder, activity)
  mrt send [-t TAB] [--shell S] [--cwd DIR] [--idle] [--json] [--] COMMAND...
                                          Run COMMAND in a tab and print its output;
                                          exits with the command's exit code. --idle
                                          picks a tab of this repository that is not
                                          running anything (or opens one)
  mrt capture [-t TAB] [-n LINES] [-b BLOCKS] [--json]
                                          Print the last blocks of a tab (default 1 block)
  mrt split [-t TAB] h|v|off [--json]     Split a tab side by side (h), stacked (v), or unsplit
  mrt quick [show|hide|toggle]            Bring MRT to the front or minimize it (default: toggle);
                                          starts MRT when it is not running. Bind it to a global
                                          hotkey in your OS for a quick terminal
  mrt quit                                Close MRT, including a window kept running in the
                                          background, and stop what still runs in it

TAB is a tab index, UUID, or title substring (default: the active tab).
Exit codes: 0 ok, the command's code for `send`, 2 usage error,
69 no MRT window running, 70 MRT reported an error.";

/// Parses the arguments after `mrt` (`args[0]` is the subcommand).
pub fn parse_args(args: &[String]) -> Result<CliInvocation, String> {
    let Some(sub) = args.first() else { return Err("missing subcommand".into()) };
    let mut target = None;
    let mut json_out = false;
    let mut shell = None;
    let mut cwd = None;
    let mut idle = false;
    let mut lines = None;
    let mut blocks = 1usize;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 1;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| format!("{flag} needs a value"))
    };
    while i < args.len() {
        let arg = args[i].as_str();
        // For `send`, everything after the first plain word is the command.
        if sub == "send" && !rest.is_empty() {
            rest.push(args[i].clone());
            i += 1;
            continue;
        }
        match arg {
            "--" => {
                rest.extend(args[i + 1..].iter().cloned());
                break;
            }
            "--json" => json_out = true,
            "-t" | "--target" => target = Some(value(&mut i, arg)?),
            "--shell" if sub == "send" => shell = Some(value(&mut i, arg)?),
            "--cwd" if sub == "send" => cwd = Some(value(&mut i, arg)?),
            "--idle" if sub == "send" => idle = true,
            "-n" | "--lines" if sub == "capture" => {
                lines = Some(value(&mut i, arg)?.parse().map_err(|_| format!("{arg} needs a number"))?)
            }
            "-b" | "--blocks" if sub == "capture" => {
                blocks = value(&mut i, arg)?.parse().map_err(|_| format!("{arg} needs a number"))?
            }
            flag if flag.starts_with('-') && flag.len() > 1 => return Err(format!("unknown option {flag} for `{sub}`")),
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }
    let command = match sub.as_str() {
        "list" if rest.is_empty() => CliCommand::List,
        "send" => {
            if rest.is_empty() {
                return Err("send needs a command".into());
            }
            // `--idle` looks for a free tab of the repository you are in.
            let cwd = match (idle, cwd) {
                (true, None) => std::env::current_dir().ok().map(|d| d.display().to_string()),
                (_, cwd) => cwd,
            };
            CliCommand::Send { command: rest.join(" "), shell, cwd, idle }
        }
        "capture" if rest.is_empty() => CliCommand::Capture { lines, blocks: blocks.max(1) },
        "split" => {
            let direction = match rest.as_slice() {
                [d] if matches!(d.as_str(), "h" | "horizontal") => SplitArg::Horizontal,
                [d] if matches!(d.as_str(), "v" | "vertical") => SplitArg::Vertical,
                [d] if matches!(d.as_str(), "off" | "none" | "unsplit") => SplitArg::Off,
                _ => return Err("split needs h, v or off".into()),
            };
            CliCommand::Split { direction }
        }
        "quick" => {
            let action = match rest.as_slice() {
                [] => "toggle".to_string(),
                [a] if matches!(a.as_str(), "show" | "hide" | "toggle") => a.clone(),
                _ => return Err("quick takes show, hide or toggle".into()),
            };
            CliCommand::Quick { action }
        }
        "quit" if rest.is_empty() => CliCommand::Quick { action: "quit".into() },
        "list" | "capture" | "quit" => return Err(format!("unexpected argument {:?}", rest[0])),
        other => return Err(format!("unknown subcommand {other}")),
    };
    Ok(CliInvocation { command, target, json: json_out })
}

/// The MCP tool and arguments for an invocation.
pub fn tool_request(inv: &CliInvocation) -> (&'static str, Value) {
    let mut args = serde_json::Map::new();
    if let Some(target) = &inv.target {
        args.insert("tab_id".into(), json!(target));
    }
    let name = match &inv.command {
        CliCommand::List => {
            args.clear();
            "mrt_list_tabs"
        }
        CliCommand::Send { command, shell, cwd, idle } => {
            args.insert("command".into(), json!(command));
            if *idle {
                args.insert("idle".into(), json!(true));
            }
            if let Some(shell) = shell {
                args.insert("shell".into(), json!(shell));
            }
            if let Some(cwd) = cwd {
                args.insert("cwd".into(), json!(cwd));
            }
            "mrt_run_command"
        }
        CliCommand::Capture { blocks, .. } => {
            args.insert("limit".into(), json!(blocks));
            // Without a target, capture the active tab rather than every tab's blocks
            // (an empty tab id resolves to the active tab).
            args.entry("tab_id").or_insert_with(|| json!(""));
            "mrt_list_blocks"
        }
        CliCommand::Split { direction } => {
            let line = match direction {
                SplitArg::Horizontal => ":split-h",
                SplitArg::Vertical => ":split-v",
                SplitArg::Off => ":unsplit",
            };
            args.insert("input_text".into(), json!(line));
            args.insert("submit_input".into(), json!(true));
            "mrt_control_gui"
        }
        CliCommand::Quick { action } => {
            args.clear();
            args.insert("window".into(), json!(action));
            "mrt_control_gui"
        }
    };
    (name, Value::Object(args))
}

fn last_lines(text: &str, n: usize) -> &str {
    let trimmed = text.trim_end_matches('\n');
    let mut start = trimmed.len();
    for _ in 0..n {
        match trimmed[..start].rfind('\n') {
            Some(i) => start = i,
            None => return trimmed,
        }
    }
    &trimmed[start + 1..]
}

/// Turns a tool result into what the CLI prints and its exit code.
pub fn render(inv: &CliInvocation, text: &str, is_error: bool) -> (String, i32) {
    let parsed: Option<Value> = serde_json::from_str(text).ok();
    if inv.json {
        let code = match (&inv.command, &parsed) {
            (CliCommand::Send { .. }, Some(v)) => v["exit_code"].as_i64().map(|c| c as i32).unwrap_or(EXIT_TOOL_ERROR),
            _ if is_error => EXIT_TOOL_ERROR,
            _ => 0,
        };
        let out = parsed.map(|v| serde_json::to_string_pretty(&v).unwrap_or_default()).unwrap_or_else(|| json!({ "error": text }).to_string());
        return (out, code);
    }
    match (&inv.command, parsed) {
        (CliCommand::List, Some(Value::Array(tabs))) => {
            let mut out = String::new();
            for tab in tabs {
                let marker = if tab["active"].as_bool() == Some(true) { "*" } else { " " };
                let state = tab["activity"]["state"].as_str().unwrap_or("idle");
                out.push_str(&format!(
                    "{marker}{:<3} {:<24} {:<10} {:<10} {}\n",
                    tab["index"].as_u64().unwrap_or(0),
                    tab["title"].as_str().unwrap_or(""),
                    tab["shell_id"].as_str().unwrap_or(""),
                    state,
                    tab["cwd"].as_str().unwrap_or(""),
                ));
            }
            (out, 0)
        }
        (CliCommand::Send { .. }, Some(v)) if v.get("exit_code").is_some() => {
            let code = v["exit_code"].as_i64().unwrap_or(1) as i32;
            (v["output"].as_str().unwrap_or("").to_string(), code)
        }
        (CliCommand::Capture { lines, .. }, Some(Value::Array(blocks))) => {
            // Blocks come newest first; print them in order.
            let mut out = String::new();
            for block in blocks.iter().rev() {
                let output = block["output"].as_str().unwrap_or("");
                let output = match lines {
                    Some(n) => last_lines(output, *n),
                    None => output.trim_end_matches('\n'),
                };
                if lines.is_none() || blocks.len() > 1 {
                    out.push_str(&format!("$ {}\n", block["command"].as_str().unwrap_or("")));
                }
                if !output.is_empty() {
                    out.push_str(output);
                    out.push('\n');
                }
            }
            (out, 0)
        }
        (CliCommand::Split { .. } | CliCommand::Quick { .. }, _) if !is_error => (String::new(), 0),
        _ => (text.to_string(), if is_error { EXIT_TOOL_ERROR } else { 0 }),
    }
}

/// Sends a `tools/call` to the running window. `Err` carries a message for stderr
/// and the exit code.
async fn call_tool(name: &str, arguments: Value) -> Result<(String, bool), (String, i32)> {
    let (port, token) = mrt_mcp::read_bridge_endpoint()
        .ok_or_else(|| ("No MRT window is running (start `mrt` first).".to_string(), EXIT_NO_WINDOW))?;
    call_tool_at(port, &token, name, arguments).await
}

async fn call_tool_at(port: u16, token: &str, name: &str, arguments: Value) -> Result<(String, bool), (String, i32)> {
    let no_window = || ("No MRT window is running (start `mrt` first).".to_string(), EXIT_NO_WINDOW);
    let request = mrt_mcp::JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": arguments })),
    };
    let response = mrt_mcp::try_forward_jsonrpc_to_gui(port, token, &request)
        .await
        .map_err(|e| (format!("No MRT window answered ({e})."), EXIT_NO_WINDOW))?
        .ok_or_else(no_window)?;
    if let Some(error) = response.error {
        return Err((error.message, EXIT_TOOL_ERROR));
    }
    let result = response.result.unwrap_or(Value::Null);
    let text = result["content"]
        .as_array()
        .map(|items| items.iter().filter_map(|i| i["text"].as_str()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    Ok((text, result["isError"].as_bool().unwrap_or(false)))
}

/// What a new `mrt` window should do about an MRT that already runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// No window answers (or only a headless `mrt server`): start normally.
    Start,
    /// The running window was brought to the front; this process exits.
    Shown,
    /// An idle window of another version was asked to quit; start (its layout is restored).
    Replaced,
    /// Another version is still running commands; it was shown and keeps running.
    OlderBusy(String),
}

/// Decides the handoff from the running window's version, whether it is a GUI, and
/// whether it is running something.
pub fn decide_handoff(running_version: &str, our_version: &str, gui_attached: bool, busy: bool) -> Handoff {
    if !gui_attached {
        Handoff::Start
    } else if running_version == our_version {
        Handoff::Shown
    } else if busy {
        Handoff::OlderBusy(running_version.to_string())
    } else {
        Handoff::Replaced
    }
}

/// Single instance and upgrades: a second `mrt` brings the running window back
/// (including one kept running in the background after it was closed). A new
/// version replaces an idle older window, and leaves one that still runs commands
/// alone until they finish.
pub fn handoff_to_running_instance() -> Handoff {
    let Some((port, token)) = mrt_mcp::read_bridge_endpoint() else { return Handoff::Start };
    let Ok(runtime) = tokio::runtime::Runtime::new() else { return Handoff::Start };
    runtime.block_on(async move {
        let initialize = mrt_mcp::JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: Some(json!({})),
        };
        let Ok(Some(response)) = mrt_mcp::try_forward_jsonrpc_to_gui(port, &token, &initialize).await else {
            return Handoff::Start;
        };
        let version = response.result.as_ref().and_then(|r| r["serverInfo"]["version"].as_str()).unwrap_or("").to_string();
        let gui_attached = match call_tool_at(port, &token, "mrt_control_gui", json!({})).await {
            Ok((text, _)) => serde_json::from_str::<Value>(&text).ok().and_then(|v| v["gui_attached"].as_bool()).unwrap_or(false),
            Err(_) => return Handoff::Start,
        };
        let busy = match call_tool_at(port, &token, "mrt_list_tabs", json!({})).await {
            Ok((text, _)) => serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .any(|t| matches!(t["activity"]["state"].as_str(), Some("running" | "progress" | "waiting_for_input"))),
            Err(_) => false,
        };
        let decision = decide_handoff(&version, env!("CARGO_PKG_VERSION"), gui_attached, busy);
        match &decision {
            Handoff::Start => {}
            Handoff::Shown | Handoff::OlderBusy(_) => {
                let _ = call_tool_at(port, &token, "mrt_control_gui", json!({ "window": "show" })).await;
            }
            Handoff::Replaced => {
                let _ = call_tool_at(port, &token, "mrt_control_gui", json!({ "window": "quit" })).await;
                // Wait for it to close (it saves its layout on the way out).
                for _ in 0..50 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let ping = mrt_mcp::JsonRpcRequest { jsonrpc: "2.0".into(), id: Some(json!(2)), method: "ping".into(), params: None };
                    if !matches!(mrt_mcp::try_forward_jsonrpc_to_gui(port, &token, &ping).await, Ok(Some(_))) {
                        break;
                    }
                }
            }
        }
        decision
    })
}

/// Runs `mrt <subcommand> ...` and returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    // `--host NAME` runs the subcommand on a paired host (`mrt pair`).
    let mut args = args.to_vec();
    let host = match args.iter().position(|a| a == "--host") {
        Some(i) if i + 1 < args.len() => {
            let host = args.remove(i + 1);
            args.remove(i);
            Some(host)
        }
        Some(_) => {
            eprintln!("mrt: --host needs a paired host name");
            return EXIT_USAGE;
        }
        None => None,
    };
    let args = &args[..];
    let inv = match parse_args(args) {
        Ok(inv) => inv,
        Err(message) => {
            eprintln!("mrt: {message}\n\n{USAGE}");
            return EXIT_USAGE;
        }
    };
    let (name, arguments) = tool_request(&inv);
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("mrt: {e}");
            return EXIT_TOOL_ERROR;
        }
    };
    let result = match &host {
        Some(host) => runtime.block_on(async {
            let mut channel = crate::remote_cli::connect(host).await.map_err(|e| (e.to_string(), EXIT_NO_WINDOW))?;
            crate::remote_cli::call_remote(&mut channel, name, arguments).await.map_err(|e| (e.to_string(), EXIT_TOOL_ERROR))
        }),
        None => runtime.block_on(call_tool(name, arguments)),
    };
    match result {
        Ok((text, is_error)) => {
            let (out, code) = render(&inv, &text, is_error);
            if code == EXIT_TOOL_ERROR && !inv.json {
                eprintln!("mrt: {}", out.trim_end());
            } else if !out.is_empty() {
                print!("{out}");
                if !out.ends_with('\n') {
                    println!();
                }
            }
            code
        }
        // A quick-terminal key with no MRT running starts one.
        Err((_, EXIT_NO_WINDOW)) if matches!(&inv.command, CliCommand::Quick { action } if action != "hide" && action != "quit") => {
            match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn()) {
                Ok(_) => 0,
                Err(e) => {
                    eprintln!("mrt: could not start MRT: {e}");
                    EXIT_NO_WINDOW
                }
            }
        }
        Err((message, code)) => {
            eprintln!("mrt: {message}");
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_subcommands_and_targets() {
        assert_eq!(parse_args(&args("list --json")).unwrap(), CliInvocation { command: CliCommand::List, target: None, json: true });
        let send = parse_args(&args("send -t build --shell bash cargo test -p x --json")).unwrap();
        assert_eq!(send.target.as_deref(), Some("build"));
        assert_eq!(
            send.command,
            CliCommand::Send { command: "cargo test -p x --json".into(), shell: Some("bash".into()), cwd: None, idle: false },
            "flags after the command belong to the command"
        );
        let send = parse_args(&args("send -- -v --version")).unwrap();
        assert_eq!(send.command, CliCommand::Send { command: "-v --version".into(), shell: None, cwd: None, idle: false });
        assert_eq!(
            parse_args(&args("capture -t 2 -n 20")).unwrap().command,
            CliCommand::Capture { lines: Some(20), blocks: 1 }
        );
        assert_eq!(parse_args(&args("split v")).unwrap().command, CliCommand::Split { direction: SplitArg::Vertical });
        assert!(parse_args(&args("split")).is_err());
        assert_eq!(parse_args(&args("quick")).unwrap().command, CliCommand::Quick { action: "toggle".into() });
        assert_eq!(
            tool_request(&parse_args(&args("quick show")).unwrap()),
            ("mrt_control_gui", json!({ "window": "show" }))
        );
        assert!(parse_args(&args("quick spin")).is_err());
        assert!(parse_args(&args("send")).is_err());
        // `--idle` uses the folder you are in to choose the repository.
        let idle = parse_args(&args("send --idle make test")).unwrap();
        let here = std::env::current_dir().unwrap().display().to_string();
        assert_eq!(idle.command, CliCommand::Send { command: "make test".into(), shell: None, cwd: Some(here.clone()), idle: true });
        assert_eq!(tool_request(&idle), ("mrt_run_command", json!({ "command": "make test", "cwd": here, "idle": true })));
        assert!(parse_args(&args("list extra")).is_err());
        assert!(parse_args(&args("capture -n many")).is_err());
        assert!(parse_args(&args("list --bogus")).is_err());
    }

    #[test]
    fn maps_to_mcp_tools() {
        let (name, a) = tool_request(&parse_args(&args("send -t 1 echo hi")).unwrap());
        assert_eq!(name, "mrt_run_command");
        assert_eq!(a, json!({ "tab_id": "1", "command": "echo hi" }));
        let (name, a) = tool_request(&parse_args(&args("capture -b 3")).unwrap());
        assert_eq!(name, "mrt_list_blocks");
        assert_eq!(a, json!({ "tab_id": "", "limit": 3 }));
        let (name, a) = tool_request(&parse_args(&args("split -t 0 h")).unwrap());
        assert_eq!(name, "mrt_control_gui");
        assert_eq!(a, json!({ "tab_id": "0", "input_text": ":split-h", "submit_input": true }));
        assert_eq!(tool_request(&parse_args(&args("list -t 3")).unwrap()), ("mrt_list_tabs", json!({})));
    }

    #[test]
    fn renders_results_and_exit_codes() {
        let send = parse_args(&args("send false")).unwrap();
        let result = json!({ "exit_code": 3, "output": "boom\n" }).to_string();
        assert_eq!(render(&send, &result, true), ("boom\n".to_string(), 3));
        let send_json = parse_args(&args("send --json false")).unwrap();
        assert_eq!(render(&send_json, &result, true).1, 3);
        assert_eq!(render(&send, "Tab not found: x", true), ("Tab not found: x".to_string(), EXIT_TOOL_ERROR));

        let capture = parse_args(&args("capture -n 2")).unwrap();
        let blocks = json!([{ "command": "seq 5", "output": "1\n2\n3\n4\n5\n" }]).to_string();
        assert_eq!(render(&capture, &blocks, false), ("4\n5\n".to_string(), 0));
        let capture = parse_args(&args("capture -b 2")).unwrap();
        let blocks = json!([{ "command": "b", "output": "B" }, { "command": "a", "output": "A\n" }]).to_string();
        assert_eq!(render(&capture, &blocks, false).0, "$ a\nA\n$ b\nB\n");

        let list = parse_args(&args("list")).unwrap();
        let tabs = json!([{ "index": 0, "title": "main", "active": true, "shell_id": "pwsh", "cwd": "C:\\x", "activity": { "state": "running" } }]).to_string();
        let (out, code) = render(&list, &tabs, false);
        assert_eq!(code, 0);
        assert!(out.starts_with("*0") && out.contains("main") && out.contains("running") && out.contains("C:\\x"), "{out}");
    }

    #[test]
    fn talks_to_a_bridge_and_rejects_a_wrong_token() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let state = mrt_mcp::MrtMcpServerState::default();
            let endpoint = mrt_mcp::start_gui_bridge_listener(state, 0, false).await.unwrap();
            let port = endpoint.addr.port();
            let (text, is_error) = call_tool_at(port, &endpoint.token, "mrt_list_tabs", json!({})).await.unwrap();
            assert!(!is_error);
            let inv = parse_args(&args("list --json")).unwrap();
            let (out, code) = render(&inv, &text, is_error);
            assert_eq!(code, 0);
            assert!(serde_json::from_str::<Value>(&out).unwrap().is_array());
            let err = call_tool_at(port, "wrong-token", "mrt_list_tabs", json!({})).await.unwrap_err();
            assert_eq!(err.1, EXIT_NO_WINDOW);
        });
    }

    #[test]
    fn handoff_decisions() {
        assert_eq!(decide_handoff("0.9.0", "0.9.0", false, true), Handoff::Start, "a headless server does not stop a window");
        assert_eq!(decide_handoff("0.9.0", "0.9.0", true, true), Handoff::Shown);
        assert_eq!(decide_handoff("0.8.0", "0.9.0", true, false), Handoff::Replaced);
        assert_eq!(decide_handoff("0.8.0", "0.9.0", true, true), Handoff::OlderBusy("0.8.0".into()));
        assert_eq!(parse_args(&args("quit")).unwrap().command, CliCommand::Quick { action: "quit".into() });
    }

    #[test]
    fn last_lines_counts_from_the_end() {
        assert_eq!(last_lines("a\nb\nc\n", 2), "b\nc");
        assert_eq!(last_lines("a\nb", 5), "a\nb");
        assert_eq!(last_lines("", 1), "");
    }
}
