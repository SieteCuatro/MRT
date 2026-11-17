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

Before opening a PR, verify that all checks pass with zero errors and zero compiler warnings. CI treats warnings as errors, so run the same command locally:

```bash
RUSTFLAGS="-D warnings" cargo test --workspace --locked
```

(In PowerShell: `$env:RUSTFLAGS = "-D warnings"; cargo test --workspace --locked`.) On Linux, install the GUI build dependencies first (see the [User Guide](docs/USER_GUIDE.md#1-install)). Some tests spawn real shells and processes, so run them on a machine where that is allowed.

- Maintain cross-platform compatibility across Windows, macOS, and Linux (`#[cfg(target_os = "...")]` where OS-specific APIs are required, and `CREATE_NO_WINDOW` for background Windows child processes).
- Keep `mrt-ai` provider-agnostic and preserve the offline heuristic fallback.
- Add or update unit tests and documentation when changing behavior.

## Which Documents to Update

| You changed… | Update |
| :--- | :--- |
| A prompt command (`:http`, `:ps`, …), keyboard shortcut, or data file | [docs/USER_GUIDE.md](docs/USER_GUIDE.md), plus [docs/TOOLBOX.md](docs/TOOLBOX.md) for tools |
| An MCP tool or resource (name, arguments, count) | [docs/MCP_GUIDE.md](docs/MCP_GUIDE.md), the tool count in [README.md](README.md), [AGENTS.md](AGENTS.md), and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| Shells, the PTY engine, block classification, or Safety Gate rules | [docs/CORE_AND_SHELLS.md](docs/CORE_AND_SHELLS.md) |
| AI providers, models, or environment variables | [docs/AI_PROVIDERS_AND_AGENTS.md](docs/AI_PROVIDERS_AND_AGENTS.md) and the table in [README.md](README.md) |
| GUI panels, widgets, or the theme | [docs/GUI_AND_WIDGETS.md](docs/GUI_AND_WIDGETS.md) |
| The output pipeline (streaming, VT parsing, classification, block rendering) in a way that changes speed or memory | Re-run `cargo bench -p mrt-core` and update [docs/PERFORMANCE.md](docs/PERFORMANCE.md) |
| A roadmap item | Mark it in [ROADMAP.md](ROADMAP.md) |
