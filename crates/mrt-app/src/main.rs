#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod history;
mod theme;
mod widgets;

use anyhow::Result;
use app::MrtGuiApp;
use mrt_core::{attach_parent_console_if_cli, hydrate_login_environment};
use mrt_mcp::{clear_bridge_port_file, run_stdio_mcp_server, MrtMcpServerState};

fn main() -> Result<()> {
    hydrate_login_environment();

    let args: Vec<String> = std::env::args().collect();

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
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let native_options = eframe::NativeOptions {
        centered: true,
        viewport: egui::ViewportBuilder::default()
            .with_title("Modern Rust Terminal")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([860.0, 520.0])
            .with_visible(true)
            .with_active(true),
        ..Default::default()
    };

    let res = eframe::run_native(
        "Modern Rust Terminal",
        native_options,
        Box::new(|_| Ok(Box::new(MrtGuiApp::new()))),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"));

    clear_bridge_port_file();
    res
}
