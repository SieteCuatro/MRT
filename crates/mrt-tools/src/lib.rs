pub mod bug_reporter;
pub mod dns_inspector;
pub mod http_inspector;
pub mod net_inspector;
pub mod structured_diff;
pub mod web_fetch;
pub mod ws_inspector;

pub use bug_reporter::{generate_markdown_bug_report, redact_sensitive_text};
pub use dns_inspector::{inspect_dns, parse_dns_query, DnsRecordEntry, DnsTraceReport};
pub use http_inspector::{execute_http_request, parse_http_command, HttpRequestSpec};
pub use net_inspector::{inspect_listening_ports, PortEntry};
pub use structured_diff::{
    canonicalize_json, compare_blocks_structured, ChangeType, JsonFieldChange, StructuredDiffKind,
    StructuredDiffReport,
};
pub use web_fetch::{fetch_url_as_markdown, html_to_readable_markdown};
pub use ws_inspector::{inspect_websocket, parse_ws_command, WsFrameEntry, WsSessionReport};
