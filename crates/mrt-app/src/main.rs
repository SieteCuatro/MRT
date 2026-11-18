#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod agent_setup;
mod app;
mod cli;
mod config_runtime;
mod copy_mode;
mod find;
mod fonts;
mod history;
mod hooks;
mod keymap;
mod layout;
mod notifications;
mod remote_cli;
mod server;
mod session_host;
mod shell_pool;
mod term_input;
mod theme;
mod widgets;
mod windows;

use anyhow::Result;
use app::MrtGuiApp;
use mrt_core::{attach_parent_console_if_cli, hydrate_login_environment};
use mrt_mcp::{clear_bridge_port_file, run_stdio_mcp_server, MrtMcpServerState};

fn main() -> Result<()> {
    hydrate_login_environment();

    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("agent") {
        attach_parent_console_if_cli();
        return agent_setup::run_cli(&args[2..]);
    }

    if let Some(sub @ ("share" | "pair" | "attach")) = args.get(1).map(String::as_str) {
        attach_parent_console_if_cli();
        std::process::exit(remote_cli::run(sub, &args[2..]));
    }

    if args.get(1).map(String::as_str) == Some("session-host") {
        return session_host::run_host_process();
    }

    if args.get(1).map(String::as_str) == Some("server") {
        attach_parent_console_if_cli();
        let rt = tokio::runtime::Runtime::new()?;
        return rt.block_on(server::run());
    }

    if args.get(1).is_some_and(|sub| cli::SUBCOMMANDS.contains(&sub.as_str())) {
        attach_parent_console_if_cli();
        std::process::exit(cli::run(&args[1..]));
    }

    if args.iter().any(|a| a == "--mcp-server") {
        let rt = tokio::runtime::Runtime::new()?;
        return rt.block_on(async {
            let state = MrtMcpServerState::default();
            run_stdio_mcp_server(state).await
        });
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        attach_parent_console_if_cli();
        println!("Modern Rust Terminal (mrt) {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        attach_parent_console_if_cli();
        println!("Modern Rust Terminal (MRT) - Cross-Platform GPU Terminal with Blocks, Toolbox (Network & Task Manager) & MCP");
        println!("\nUsage:");
        println!("  mrt                 Launch the native GPU terminal GUI");
        println!("  mrt --mcp-server    Run MRT in headless Model Context Protocol (MCP) stdio server mode");
        println!("  mrt --version       Print version information");
        println!("  mrt agent setup     Register MRT's MCP server and skill with Claude Code, Codex CLI and Gemini CLI");
        println!("                      [--agent claude|codex|gemini|all] [--user|--project] [--dry-run]");
        println!("  mrt agent show      Print the MCP command line and agent instructions for other agents");
        println!("  mrt agent remove    Undo `mrt agent setup` (same flags)");
        println!();
        println!("{}", cli::USAGE);
        println!();
        println!("{}", remote_cli::USAGE);
        println!();
        println!("  mrt server          Run a headless session server (tabs, blocks, PTYs) for the CLI and agents");
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // One MRT window per user: bring back the running one (also after it was closed
    // with work still running), or replace an idle older version.
    match cli::handoff_to_running_instance() {
        cli::Handoff::Shown => return Ok(()),
        cli::Handoff::OlderBusy(version) => {
            attach_parent_console_if_cli();
            eprintln!("MRT {version} is still running commands; it was brought to the front. Start MRT again when they finish to switch to {}.", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        cli::Handoff::Start | cli::Handoff::Replaced => {}
    }

    let native_options = eframe::NativeOptions {
        centered: true,
        viewport: egui::ViewportBuilder::default()
            .with_title("Modern Rust Terminal")
            // Window and taskbar icon; on Linux the app id matches the desktop
            // entry (`StartupWMClass=mrt`, `Icon=mrt`).
            .with_icon(std::sync::Arc::new(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/mrt-256.png")).unwrap_or_default(),
            ))
            .with_app_id("mrt")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([860.0, 520.0])
            .with_visible(true)
            .with_active(true),
        ..Default::default()
    };

    let res = eframe::run_native(
        "Modern Rust Terminal",
        native_options,
        Box::new(|cc| {
            app::set_ui_context(&cc.egui_ctx);
            Ok(Box::new(MrtGuiApp::new()))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"));

    clear_bridge_port_file();
    res
}
