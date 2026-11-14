pub mod block;
pub mod platform;
pub mod pty;
pub mod safety;
pub mod vt;

pub use block::{
    classify_output, CommandBlock, HttpExchangeSummary, StructuredOutput, ViewMode,
};
pub use platform::{attach_parent_console_if_cli, hydrate_login_environment};
pub use pty::{
    discover_shells, execute_command_in_shell, execute_command_streaming, InteractivePty, ShellKind, ShellProfile,
};
pub use safety::{assess_command, RiskLevel, SafetyAssessment};
pub use vt::{parse_ansi_text, AnsiColor, AnsiSpan, VtParserState};
