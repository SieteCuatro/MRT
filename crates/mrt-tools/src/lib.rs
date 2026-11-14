pub mod http_inspector;
pub mod net_inspector;
pub mod web_fetch;

pub use http_inspector::{execute_http_request, parse_http_command, HttpRequestSpec};
pub use net_inspector::{inspect_listening_ports, PortEntry};
pub use web_fetch::{fetch_url_as_markdown, html_to_readable_markdown};
