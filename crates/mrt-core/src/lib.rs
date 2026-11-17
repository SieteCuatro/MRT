pub mod block;
pub mod links;
pub mod platform;
pub mod pty;
pub mod safety;
pub mod shell_integration;
pub mod vt;

pub use block::{
    classify_output, ClipboardDecision, ClipboardRequest, CommandBlock, HttpExchangeSummary,
    StructuredOutput, ViewMode,
};
pub use platform::{attach_parent_console_if_cli, hydrate_login_environment};
pub use pty::{
    discover_shells, execute_command_in_shell, execute_command_streaming,
    execute_command_streaming_with_pid, InteractivePty, ShellKind, ShellProfile,
};
pub use safety::{
    assess_command, is_protected_process_name, normalize_process_name, RiskLevel,
    SafetyAssessment, PROTECTED_PROCESS_NAMES,
};
pub use shell_integration::{
    generate_shell_integration_script, parse_osc133_markers, Osc133Event, Osc133Kind,
};
pub use links::{classify_uri, detect_links, is_openable_uri, DetectedLink, LinkTarget};
pub use vt::{
    parse_ansi_rich, parse_ansi_text, parse_osc7_uri, AnsiColor, AnsiSpan, ParsedAnsi, VtEvent,
    VtParserState,
};
