pub mod activity;
pub mod block;
pub mod config;
pub mod images;
pub mod links;
pub mod platform;
pub mod pty;
pub mod safety;
pub mod shell_integration;
pub mod term;
pub mod theme_file;
pub mod shell_session;
pub mod vt;

pub use activity::{
    detect_agent_cli, parse_activity_signals, ActivitySignal, ActivitySnapshot, ActivityState,
    ActivityTracker, ProgressTone, MRT_STATUS_OSC,
};
pub use block::{
    classify_output, max_block_output_bytes, set_max_block_output_bytes, ClipboardDecision,
    ClipboardRequest, CommandBlock, HttpExchangeSummary, StructuredOutput, ViewMode,
    DEFAULT_MAX_BLOCK_OUTPUT_BYTES, STRUCTURED_CLASSIFY_LIMIT,
};
pub use config::{
    load_config, ConfigError, ConfigSources, ConfigWatcher, HookEntry, LoadedConfig,
    McpServerEntry, MrtConfig, NamedActionEntry, NotificationsConfig, SafetyConfig, HOOK_EVENTS,
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
pub use links::{classify_uri, detect_links, is_openable_uri, DetectedLink, LinkTarget};
pub use theme_file::{load_theme_file, parse_hex_color, parse_theme, set_ansi_palette, ThemeSpec};
pub use images::{decode_image_file, ImageSize, InlineImage};
pub use vt::{
    parse_ansi_rich, parse_ansi_text, parse_osc7_uri, AnsiColor, AnsiSpan, ParsedAnsi, VtEvent,
    VtParserState, VtStream,
};
pub use term::{
    xterm_palette, MouseMode, TermCell, TermColor, TermCursor, TermCursorShape, TermModes,
    TermSnapshot, TerminalEmulator,
};
