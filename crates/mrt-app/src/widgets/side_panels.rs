use egui::{Color32, RichText, Ui};
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
        }
    }
}

pub fn render_mcp_side_panel(
    ui: &mut Ui,
    state: &mut SidePanelState,
    mcp_host: &McpHostManager,
) -> Option<SidePanelEvent> {
    let mut event = None;

    ui.heading(
        RichText::new("🔌 Model Context Protocol (MCP) Hub")
            .size(15.0)
            .strong()
            .color(Color32::from_rgb(140, 205, 255)),
    );
    ui.label(
        RichText::new("MRT acts as both an MCP Host (connecting external tool servers) and an MCP Server (`mrt --mcp-server`).")
            .size(11.5)
            .color(Color32::from_rgb(165, 175, 190)),
    );
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(RichText::new("Connected MCP Servers").strong());
        for srv in &mcp_host.servers {
            egui::Frame::group(ui.style())
                .fill(Color32::from_rgb(24, 28, 36))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let dot_color = if srv.connected {
                            Color32::from_rgb(110, 235, 150)
                        } else {
                            Color32::from_rgb(245, 110, 120)
                        };
                        ui.label(RichText::new("●").color(dot_color));
                        ui.label(RichText::new(&srv.config.name).strong());
                        ui.label(
                            RichText::new(format!("v{}", srv.server_version))
                                .size(11.0)
                                .color(Color32::GRAY),
                        );
                    });
                    ui.label(
                        RichText::new(format!("Transport: {}", srv.config.command))
                            .size(11.0)
                            .monospace()
                            .color(Color32::from_rgb(145, 155, 170)),
                    );
                    if let Some(ref err) = srv.last_error {
                        ui.label(
                            RichText::new(format!("Error: {err}"))
                                .size(11.0)
                                .color(Color32::from_rgb(255, 140, 140)),
                        );
                    }

                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Available Tools ({})", srv.tools.len()))
                            .size(11.5)
                            .strong()
                            .color(Color32::from_rgb(150, 205, 250)),
                    );

                    for tool in &srv.tools {
                        egui::CollapsingHeader::new(
                            RichText::new(&tool.name)
                                .monospace()
                                .color(Color32::from_rgb(175, 235, 185)),
                        )
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(RichText::new(&tool.description).size(11.5));
                            if ui.small_button("▶ Invoke Tool").clicked() {
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
        ui.label(RichText::new("Custom Tool Arguments (JSON)").strong());
        ui.add(
            egui::TextEdit::multiline(&mut state.mcp_tool_args_input)
                .font(egui::TextStyle::Monospace)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );

        ui.add_space(8.0);
        ui.separator();
        ui.label(RichText::new("Add External Stdio MCP Server").strong());
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut state.new_mcp_name);
        });
        ui.horizontal(|ui| {
            ui.label("Command:");
            ui.text_edit_singleline(&mut state.new_mcp_cmd);
        });
        ui.horizontal(|ui| {
            ui.label("Args:");
            ui.text_edit_singleline(&mut state.new_mcp_args);
        });
        if ui.button("➕ Connect MCP Server").clicked() {
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

    ui.heading(
        RichText::new("🌐 Internet & Network Tools Studio")
            .size(15.0)
            .strong()
            .color(Color32::from_rgb(130, 225, 195)),
    );
    ui.separator();

    ui.label(RichText::new("1. Inline HTTP / REST API Inspector (`:http`)").strong());
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
        ui.label(RichText::new("Request JSON Body:").size(11.5));
        ui.add(
            egui::TextEdit::multiline(&mut state.http_body)
                .font(egui::TextStyle::Monospace)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
    }
    if ui.button("🚀 Send HTTP Request to Terminal Block").clicked() {
        event = Some(SidePanelEvent::RunHttpInspector {
            method: state.http_method.clone(),
            url: state.http_url.clone(),
            body: state.http_body.clone(),
        });
    }

    ui.add_space(12.0);
    ui.separator();
    ui.label(RichText::new("2. Readable Web Page Extractor (`:web`)").strong());
    ui.add(
        egui::TextEdit::singleline(&mut state.web_url)
            .hint_text("https://www.rust-lang.org")
            .desired_width(f32::INFINITY),
    );
    if ui.button("📖 Fetch & Render Webpage as Markdown").clicked() {
        event = Some(SidePanelEvent::RunWebReader {
            url: state.web_url.clone(),
        });
    }

    ui.add_space(12.0);
    ui.separator();
    ui.label(RichText::new("3. Local Network & Listening Ports (`:ports`)").strong());
    ui.label(
        RichText::new("Scans active local TCP/UDP listening sockets and PIDs into a sortable table block.")
            .size(11.5)
            .color(Color32::GRAY),
    );
    if ui.button("🔍 Scan Active Listening Ports").clicked() {
        event = Some(SidePanelEvent::RunPortScanner);
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
