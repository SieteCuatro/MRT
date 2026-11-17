pub mod bug_reporter;
pub mod dns_inspector;
pub mod http_inspector;
pub mod net_inspector;
pub mod process_control;
pub mod process_inspector;
pub mod service_inspector;
pub mod structured_diff;
pub mod web_fetch;
pub mod ws_inspector;

pub use bug_reporter::{generate_markdown_bug_report, redact_sensitive_text};
pub use dns_inspector::{inspect_dns, parse_dns_query, DnsRecordEntry, DnsTraceReport};
pub use http_inspector::{execute_http_request, parse_http_command, HttpRequestSpec};
pub use net_inspector::{inspect_listening_ports, list_listening_ports, PortEntry};
pub use process_inspector::{
    compare_process_snapshots, entries_from_table, format_bytes, inspect_process,
    is_process_block, kill_outcomes_markdown, owner_label, parse_kill_command, parse_ps_command,
    parse_watch_command, plan_kill, process_details_markdown, processes_to_table,
    query_processes, snapshot_summary, KillOutcome, KillPlan, KillRequest, KillStatus,
    OwnedProcess, OwnedProcessKind, OwnedProcessMap, ProcessDetails, ProcessDiffReport,
    ProcessEntry, ProcessQuery, ProcessRow, ProcessSelector, ProcessSnapshot, ProcessSort,
    ProcessWatch, ProcessWatchSpec, SharedProcessSampler, WatchAlert, PROCESS_BLOCK_SHELL,
};
pub use structured_diff::{
    canonicalize_json, compare_blocks_structured, ChangeType, JsonFieldChange, StructuredDiffKind,
    StructuredDiffReport,
};
pub use web_fetch::{fetch_url_as_markdown, html_to_readable_markdown};
pub use ws_inspector::{inspect_websocket, parse_ws_command, WsFrameEntry, WsSessionReport};
pub use process_control::{
    apply_control, control_outcomes_markdown, parse_control_command, plan_control, ControlAction, ControlOutcome,
    ControlRequest, Priority,
};
pub use service_inspector::{
    list_services, parse_service_command, service_shell_command, services_to_table, ServiceAction, ServiceEntry,
};
