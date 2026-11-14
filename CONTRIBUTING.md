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

Before opening a PR, verify that all checks pass with zero errors and zero compiler warnings:

```bash
cargo check --workspace
cargo test --workspace
```

- Maintain cross-platform compatibility across Windows, macOS, and Linux (`#[cfg(target_os = "...")]` where OS-specific APIs are required, and `CREATE_NO_WINDOW` for background Windows child processes).
- Keep `mrt-ai` provider-agnostic and preserve the offline heuristic fallback.
- Add or update unit tests and documentation when changing behavior.
