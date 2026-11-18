# Contributing to Modern Rust Terminal (MRT)

MRT is an open-source MIT-licensed project. Contributions from both human developers and AI-assisted workflows are welcome.

## Getting Started

1. Fork and clone the repository:
   ```bash
   git clone https://github.com/SieteCuatro/MRT.git
   cd MRT
   ```
2. Review [AGENTS.md](AGENTS.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) to understand the 5-crate workspace layout (`mrt-core`, `mrt-tools`, `mrt-mcp`, `mrt-ai`, `mrt-app`) and coding conventions.
3. Check [ROADMAP.md](ROADMAP.md) for open milestones and project ideas.
4. Implement your change with unit tests and open a pull request describing the behavior and verification steps.

## Pull Request Checklist

Before opening a PR, verify that all checks pass with zero errors and zero compiler warnings. The CI workflow is currently disabled (no GitHub Actions minutes), so this local run is the check; it uses the same settings CI does:

```bash
RUSTFLAGS="-D warnings" cargo test --workspace --locked
```

(In PowerShell: `$env:RUSTFLAGS = "-D warnings"; cargo test --workspace --locked`.) On Linux, install the GUI build dependencies first (see the [User Guide](docs/USER_GUIDE.md#1-install)). Some tests spawn real shells and processes, so run them on a machine where that is allowed.

`crates/mrt-app/tests/e2e_processes.rs` starts real `mrt` processes (`server`, `share`, `pair`, `--host`, `session-host`), each with its own `MRT_DATA_DIR`, so it never touches your own MRT data. To check the real window without a desktop session, `scripts/e2e/` runs it on an offscreen X server (`xvfb_offscreen.sh`, no root needed) and drives it over MCP: `gui_crash_reattach.py` (kill the window, reopen, check the programs come back) and `gui_feature_tour.py` (screenshots of several features).

- Maintain cross-platform compatibility across Windows, macOS, and Linux (`#[cfg(target_os = "...")]` where OS-specific APIs are required, and `CREATE_NO_WINDOW` for background Windows child processes).
- Keep `mrt-ai` provider-agnostic and preserve the offline heuristic fallback.
- Add or update unit tests and documentation when changing behavior.

## Which Documents to Update

| You changed… | Update |
| :--- | :--- |
| A prompt command (`:http`, `:ps`, …), keyboard shortcut, or data file | [docs/USER_GUIDE.md](docs/USER_GUIDE.md), plus [docs/TOOLBOX.md](docs/TOOLBOX.md) for tools |
| Something a new user meets in the first steps (install, the prompt, tabs, AI setup, settings) | [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) and its Spanish version [docs/GUIA_DE_INICIO.md](docs/GUIA_DE_INICIO.md) |
| An MCP tool or resource (name, arguments, count) | [docs/MCP_GUIDE.md](docs/MCP_GUIDE.md), the tool count in [README.md](README.md), [AGENTS.md](AGENTS.md), and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| Shells, the PTY engine, block classification, or Safety Gate rules | [docs/CORE_AND_SHELLS.md](docs/CORE_AND_SHELLS.md) |
| AI providers, models, or environment variables | [docs/AI_PROVIDERS_AND_AGENTS.md](docs/AI_PROVIDERS_AND_AGENTS.md) and the table in [README.md](README.md) |
| GUI panels, widgets, or the theme | [docs/GUI_AND_WIDGETS.md](docs/GUI_AND_WIDGETS.md) |
| The output pipeline (streaming, VT parsing, classification, block rendering) in a way that changes speed or memory | Re-run `cargo bench -p mrt-core` and update [docs/PERFORMANCE.md](docs/PERFORMANCE.md) |
| A config setting | [mrt.example.toml](mrt.example.toml) (a test parses it) and the settings table in [docs/USER_GUIDE.md](docs/USER_GUIDE.md#6-configuration-file) |
| The session host, background mode, `mrt share` / `pair` / `attach`, or the phone companion | [docs/SESSION_SERVER.md](docs/SESSION_SERVER.md) and [docs/USER_GUIDE.md](docs/USER_GUIDE.md) |
| A module, crate dependency, or process | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [AGENTS.md](AGENTS.md) |
| A roadmap item | Mark it in [ROADMAP.md](ROADMAP.md) |

## Building release archives

While the release workflow is disabled, archives are built locally:

- **Windows:** `cargo build --release --locked -p mrt-app`, then zip `target/release/mrt.exe` as `mrt-<tag>-x86_64-pc-windows-msvc.zip`.
- **Linux:** `scripts/release/build-linux.sh <tag>` (in WSL or on Linux, with Zig and `cargo-zigbuild` installed). It links against glibc 2.17, checks that the binary asks for nothing newer, and writes `mrt-<tag>-x86_64-unknown-linux-gnu.tar.gz`. Do not ship a plain `cargo build` binary for Linux: it needs the build machine's glibc or newer.
