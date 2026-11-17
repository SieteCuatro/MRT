pub mod block;
pub mod platform;
pub mod pty;
pub mod safety;
pub mod shell_integration;
pub mod shell_session;
pub mod vt;

pub use block::{
    classify_output, CommandBlock, HttpExchangeSummary, StructuredOutput, ViewMode,
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
    generate_shell_integration_script, parse_osc133_markers, parse_osc7_path, Osc133Event,
    Osc133Kind,
};
pub use shell_session::{
    CommandEvent, CommandHandle, CommandOutcome, ShellSession, ShellSessionError,
    ShellSessionOptions,
};
pub use vt::{parse_ansi_text, AnsiColor, AnsiSpan, VtParserState};
