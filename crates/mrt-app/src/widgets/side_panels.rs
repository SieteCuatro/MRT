use crate::theme::Palette;
use egui::{RichText, Ui};
use mrt_mcp::McpHostManager;
use serde_json::Value;

pub enum SidePanelEvent {
    InvokeMcpTool {
        server_name: String,
        tool_name: String,
        args_json: String,
    },
    ConnectExternalMcp {
        name: String,
        command: String,
        args: String,
    },
    RunHttpInspector {
        method: String,
        url: String,
        body: String,
    },
    RunWebReader {
        url: String,
    },
    RunPortScanner,
    RunDnsTrace {
        host: String,
    },
    RunWsInspector {
        url: String,
        msg: String,
    },
}

pub struct SidePanelState {
    pub show_mcp_panel: bool,
    pub show_internet_panel: bool,
    pub new_mcp_name: String,
    pub new_mcp_cmd: String,
    pub new_mcp_args: String,
    pub mcp_tool_args_input: String,
    pub http_method: String,
    pub http_url: String,
    pub http_body: String,
    pub web_url: String,
    pub dns_host: String,
    pub ws_url: String,
    pub ws_msg: String,
}

impl Default for SidePanelState {
    fn default() -> Self {
        Self {
            show_mcp_panel: false,
            show_internet_panel: false,
            new_mcp_name: "custom-mcp".to_string(),
            new_mcp_cmd: "mrt".to_string(),
            new_mcp_args: "--mcp-server".to_string(),
            mcp_tool_args_input: "{}".to_string(),
            http_method: "GET".to_string(),
            http_url: "https://httpbin.org/json".to_string(),
            http_body: String::new(),
            web_url: "https://www.rust-lang.org".to_string(),
            dns_host: "google.com".to_string(),
            ws_url: "wss://echo.websocket.events".to_string(),
            ws_msg: "Hello from MRT!".to_string(),
        }
    }
}

pub fn render_mcp_side_panel(
    ui: &mut Ui,
    state: &mut SidePanelState,
    mcp_host: &McpHostManager,
    bridge_port: Option<u16>,
) -> Option<SidePanelEvent> {
    let mut event = None;

    ui.label(
        RichText::new("MCP Hub")
            .size(15.0)
            .strong()
            .color(Palette::TEXT_PRIMARY),
    );
    ui.label(
        RichText::new("Dual-mode MCP Host & Server (mrt --mcp-server)")
            .size(11.5)
            .color(Palette::TEXT_SECONDARY),
    );
    if let Some(port) = bridge_port {
        ui.add_space(4.0);
        egui::Frame::none()
            .fill(Palette::BG_ELEVATED)
            .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("●")
                            .size(11.0)
                            .color(Palette::ACCENT_GREEN),
                    );
                    ui.label(
                        RichText::new(format!("GUI Bridge 127.0.0.1:{port}"))
                            .size(11.5)
                            .monospace()
                            .color(Palette::TEXT_PRIMARY),
                    );
                    if ui.small_button("Copy Config").clicked() {
                        let exe = std::env::current_exe()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| "mrt".to_string());
                        let cfg = serde_json::json!({
                            "mcpServers": {
                                "modern-rust-terminal": {
                                    "command": exe,
                                    "args": ["--mcp-server"]
                                }
                            }
                        });
                        ui.output_mut(|o| {
                            o.copied_text = serde_json::to_string_pretty(&cfg).unwrap_or_default()
                        });
                    }
                });
            });
    }
    ui.add_space(4.0);
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(
            RichText::new("Connected Servers")
                .size(12.0)
                .strong()
                .color(Palette::TEXT_SECONDARY),
        );
        ui.add_space(4.0);
        for srv in &mcp_host.servers {
            egui::Frame::none()
                .fill(Palette::BG_SURFACE)
                .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::same(10.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let dot_color = if srv.connected {
                            Palette::ACCENT_GREEN
                        } else {
                            Palette::ACCENT_RED
                        };
                        ui.label(RichText::new("●").size(11.0).color(dot_color));
                        ui.label(
                            RichText::new(&srv.config.name)
                                .strong()
                                .color(Palette::TEXT_PRIMARY),
                        );
                        ui.label(
                            RichText::new(format!("v{}", srv.server_version))
                                .size(11.0)
                                .monospace()
                                .color(Palette::TEXT_MUTED),
                        );
                    });
                    ui.label(
                        RichText::new(&srv.config.command)
                            .size(11.0)
                            .monospace()
                            .color(Palette::TEXT_SECONDARY),
                    );
                    if let Some(ref err) = srv.last_error {
                        ui.label(
                            RichText::new(format!("Error: {err}"))
                                .size(11.0)
                                .color(Palette::ACCENT_RED),
                        );
                    }

                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Tools ({})", srv.tools.len()))
                            .size(11.5)
                            .strong()
                            .color(Palette::ACCENT_BLUE),
                    );

                    for tool in &srv.tools {
                        egui::CollapsingHeader::new(
                            RichText::new(&tool.name)
                                .size(12.0)
                                .monospace()
                                .color(Palette::TEXT_PRIMARY),
                        )
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&tool.description)
                                    .size(11.5)
                                    .color(Palette::TEXT_SECONDARY),
                            );
                            ui.add_space(4.0);
                            if ui.small_button("Invoke").clicked() {
                                let default_args = default_args_for_tool(&tool.name);
                                event = Some(SidePanelEvent::InvokeMcpTool {
                                    server_name: srv.config.name.clone(),
                                    tool_name: tool.name.clone(),
                                    args_json: default_args,
                                });
                            }
                        });
                    }
                });
            ui.add_space(6.0);
        }

        ui.separator();
        ui.label(
            RichText::new("Tool Arguments (JSON)")
                .size(12.0)
                .strong()
                .color(Palette::TEXT_SECONDARY),
        );
        ui.add(
            egui::TextEdit::multiline(&mut state.mcp_tool_args_input)
                .font(egui::TextStyle::Monospace)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );

        ui.add_space(8.0);
        ui.separator();
        ui.label(
            RichText::new("Server Presets")
                .size(12.0)
                .strong()
                .color(Palette::TEXT_SECONDARY),
        );
        ui.horizontal_wrapped(|ui| {
            let presets = [
                ("Filesystem", "npx", "-y @modelcontextprotocol/server-filesystem ."),
                ("Git", "npx", "-y @modelcontextprotocol/server-git"),
                ("Memory", "npx", "-y @modelcontextprotocol/server-memory"),
                ("SQLite", "npx", "-y @modelcontextprotocol/server-sqlite --db ./mrt.db"),
                ("Fetch", "uvx", "mcp-server-fetch"),
            ];
            for (name, cmd, args) in presets {
                if ui.small_button(name).on_hover_text(format!("Load preset: {cmd} {args}")).clicked() {
                    state.new_mcp_name = format!("{name}-server").to_lowercase();
                    state.new_mcp_cmd = cmd.to_string();
                    state.new_mcp_args = args.to_string();
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.label(
            RichText::new("Connect External Stdio Server")
                .size(12.0)
                .strong()
                .color(Palette::TEXT_SECONDARY),
        );
        ui.horizontal(|ui| {
            ui.label(RichText::new("Name").size(11.5).color(Palette::TEXT_MUTED));
            ui.text_edit_singleline(&mut state.new_mcp_name);
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Cmd").size(11.5).color(Palette::TEXT_MUTED));
            ui.text_edit_singleline(&mut state.new_mcp_cmd);
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Args").size(11.5).color(Palette::TEXT_MUTED));
            ui.text_edit_singleline(&mut state.new_mcp_args);
        });
        ui.add_space(4.0);
        if ui.button("Connect Server").clicked() {
            event = Some(SidePanelEvent::ConnectExternalMcp {
                name: state.new_mcp_name.clone(),
                command: state.new_mcp_cmd.clone(),
                args: state.new_mcp_args.clone(),
            });
        }
    });

    event
}

pub fn render_internet_side_panel(
    ui: &mut Ui,
    state: &mut SidePanelState,
) -> Option<SidePanelEvent> {
    let mut event = None;

    ui.label(
        RichText::new("Internet & Network")
            .size(15.0)
            .strong()
            .color(Palette::TEXT_PRIMARY),
    );
    ui.label(
        RichText::new("Built-in HTTP client, web reader, and port scanner")
            .size(11.5)
            .color(Palette::TEXT_SECONDARY),
    );
    ui.separator();

    ui.label(
        RichText::new("HTTP Inspector (:http)")
            .size(12.0)
            .strong()
            .color(Palette::ACCENT_BLUE),
    );
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("http_method_combo")
            .selected_text(&state.http_method)
            .width(75.0)
            .show_ui(ui, |ui| {
                for m in ["GET", "POST", "PUT", "DELETE", "PATCH"] {
                    ui.selectable_value(&mut state.http_method, m.to_string(), m);
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut state.http_url)
                .hint_text("https://...")
                .desired_width(f32::INFINITY),
        );
    });
    if state.http_method != "GET" {
        ui.label(
            RichText::new("Request Body (JSON)")
                .size(11.5)
                .color(Palette::TEXT_SECONDARY),
        );
        ui.add(
            egui::TextEdit::multiline(&mut state.http_body)
                .font(egui::TextStyle::Monospace)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
    }
    if ui.button("Send Request").clicked() {
        event = Some(SidePanelEvent::RunHttpInspector {
            method: state.http_method.clone(),
            url: state.http_url.clone(),
            body: state.http_body.clone(),
        });
    }

    ui.add_space(10.0);
    ui.separator();
    ui.label(
        RichText::new("Web Reader (:web)")
            .size(12.0)
            .strong()
            .color(Palette::ACCENT_BLUE),
    );
    ui.add(
        egui::TextEdit::singleline(&mut state.web_url)
            .hint_text("https://www.rust-lang.org")
            .desired_width(f32::INFINITY),
    );
    if ui.button("Fetch as Markdown").clicked() {
        event = Some(SidePanelEvent::RunWebReader {
            url: state.web_url.clone(),
        });
    }

    ui.add_space(10.0);
    ui.separator();
    ui.label(
        RichText::new("Listening Ports (:ports)")
            .size(12.0)
            .strong()
            .color(Palette::ACCENT_BLUE),
    );
    ui.label(
        RichText::new("Scans active local TCP/UDP listening sockets and PIDs.")
            .size(11.5)
            .color(Palette::TEXT_SECONDARY),
    );
    if ui.button("Scan Ports").clicked() {
        event = Some(SidePanelEvent::RunPortScanner);
    }

    ui.add_space(10.0);
    ui.separator();
    ui.label(
        RichText::new("DNS & Latency Trace (:dns)")
            .size(12.0)
            .strong()
            .color(Palette::ACCENT_BLUE),
    );
    ui.label(
        RichText::new("Resolves IPv4/IPv6 addresses and measures TCP handshake latency.")
            .size(11.5)
            .color(Palette::TEXT_SECONDARY),
    );
    ui.add(
        egui::TextEdit::singleline(&mut state.dns_host)
            .hint_text("google.com, api.github.com:443")
            .desired_width(f32::INFINITY),
    );
    if ui.button("Trace Host").clicked() {
        event = Some(SidePanelEvent::RunDnsTrace {
            host: state.dns_host.clone(),
        });
    }

    ui.add_space(10.0);
    ui.separator();
    ui.label(
        RichText::new("WebSocket Inspector (:ws)")
            .size(12.0)
            .strong()
            .color(Palette::ACCENT_BLUE),
    );
    ui.label(
        RichText::new("Inspects live WebSocket streams and sends probe frames.")
            .size(11.5)
            .color(Palette::TEXT_SECONDARY),
    );
    ui.add(
        egui::TextEdit::singleline(&mut state.ws_url)
            .hint_text("wss://echo.websocket.events")
            .desired_width(f32::INFINITY),
    );
    ui.add(
        egui::TextEdit::singleline(&mut state.ws_msg)
            .hint_text("Optional message to send...")
            .desired_width(f32::INFINITY),
    );
    if ui.button("Inspect WebSocket").clicked() {
        event = Some(SidePanelEvent::RunWsInspector {
            url: state.ws_url.clone(),
            msg: state.ws_msg.clone(),
        });
    }

    event
}

fn default_args_for_tool(tool_name: &str) -> String {
    match tool_name {
        "mrt_run_command" => serde_json::to_string(&Value::Object(
            [("command".to_string(), Value::String("echo Hello from MRT MCP".to_string()))]
                .into_iter()
                .collect(),
        ))
        .unwrap_or_else(|_| "{}".into()),
        "mrt_create_tab" => serde_json::to_string(&Value::Object(
            [("title".to_string(), Value::String("Agent Workspace".to_string()))]
                .into_iter()
                .collect(),
        ))
        .unwrap_or_else(|_| "{}".into()),
        "mrt_pty_spawn" => serde_json::to_string(&Value::Object(
            [("command".to_string(), Value::String("echo Interactive PTY ready".to_string()))]
                .into_iter()
                .collect(),
        ))
        .unwrap_or_else(|_| "{}".into()),
        "mrt_ai_translate" => serde_json::to_string(&Value::Object(
            [("query".to_string(), Value::String("list all files".to_string()))]
                .into_iter()
                .collect(),
        ))
        .unwrap_or_else(|_| "{}".into()),
        "mrt_http_request" => serde_json::to_string(&Value::Object(
            [
                ("method".to_string(), Value::String("GET".to_string())),
                ("url".to_string(), Value::String("https://httpbin.org/json".to_string())),
            ]
            .into_iter()
            .collect(),
        ))
        .unwrap_or_else(|_| "{}".into()),
        "mrt_web_fetch" => serde_json::to_string(&Value::Object(
            [("url".to_string(), Value::String("https://example.com".to_string()))]
                .into_iter()
                .collect(),
        ))
        .unwrap_or_else(|_| "{}".into()),
        _ => "{}".to_string(),
    }
}
