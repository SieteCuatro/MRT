# Performance & Throughput Benchmarks

This page explains how to measure how fast MRT handles command output, what the numbers were when the benchmarks were added, where the time goes with large output, and which fixes would help most.

Command output takes this path:

```text
child process ──pipe──▶ execute_command_streaming_with_pid   (mrt-core/src/pty.rs: read + UTF-8 decode)
                        │ on_output(chunk)
                        ▼
              GUI: process_async_events, once per frame     (mrt-app/src/app.rs)
                CommandBlock::append_output(chunk, &mut VtStream)
                (only the new chunk is parsed; output over the block limit is dropped)
                        │
                        ▼
              render_ansi_spans → one egui LayoutJob for the whole block (widgets/block_view.rs)
                        │ when the command exits
                        ▼
              CommandBlock::new_completed: parse_ansi_text + classify_output (mrt-core/src/block.rs)
              → cloned into the tab, the history worker (SQLite) and the MCP state mirror
```

---

## 1. Running the benchmarks locally

Running locally is the main way to use the benchmarks. The suite is in [`crates/mrt-core/benches/throughput.rs`](../crates/mrt-core/benches/throughput.rs) and uses [criterion](https://docs.rs/criterion).

```bash
# Whole suite (about 10–15 minutes; streaming through PowerShell is the slow part on Windows)
cargo bench -p mrt-core --bench throughput

# One group, using a regular expression on the benchmark name
cargo bench -p mrt-core --bench throughput -- vt_parse
cargo bench -p mrt-core --bench throughput -- "classify_output|block_new_completed"

# Quick check that every benchmark runs, one iteration each (what you want before a PR)
cargo bench -p mrt-core --bench throughput -- --test
```

Criterion stores results in `target/criterion/` and compares each run with the previous one, printing `Performance has regressed` or `improved` with a p-value. To compare a change against `main`:

```bash
git switch main
cargo bench -p mrt-core --bench throughput -- --save-baseline main
git switch my-branch
cargo bench -p mrt-core --bench throughput -- --baseline main
```

Close other heavy programs, including parallel `cargo` builds, before you trust absolute numbers. On a loaded machine, compare only runs made back to back.

### Benchmark groups

| Group | What it measures |
| :--- | :--- |
| `vt_parse/{plain,colored}_10MB` | `vt::parse_ansi_text` on 10 MB of log lines, without color and with an SGR change on almost every token (about half the bytes are escapes, like `ls --color` or `cargo` output). |
| `classify_output/{json,csv,log,log_comma_header}_10MB` | `block::classify_output` on a 10 MB JSON array, a 10 MB CSV table, and 10 MB of plain log lines (the last variant has a comma in line 1, so the table detector starts splitting). |
| `block_new_completed/{colored,json}_10MB` | `CommandBlock::new_completed`, the parse, classify, and copy work done when a command exits. |
| `gui_reparse_per_frame_64KiB/{1,2,4,8}MB` | A copy of the GUI's streaming loop: append a 64 KiB chunk to `raw_output`, then parse the whole output again. Throughput falls as the output grows, because the total work grows with the square of the output size. |
| `streaming/<shell>/{plain,color}_<N>MB` | End to end: `execute_command_streaming_with_pid` runs a command that prints N MB, and the result is reported in MB/s. 100 MB through `cmd` / `sh` / `bash`, and 16 MB through PowerShell (which is much slower, see below). |

The streaming generator is the benchmark binary itself, re-run through the shell as `throughput --mrt-emit <bytes> [plain|color]`. Shell loops such as `yes`, `seq`, or PowerShell `for` are slower than MRT, so they would measure the generator instead of MRT. Shells that are not installed are skipped with a message. On Windows, `cmd` is skipped if the path to the benchmark binary contains spaces.

### CI

[`.github/workflows/bench.yml`](../.github/workflows/bench.yml) runs the suite on Ubuntu, Windows, and macOS **only when someone starts it by hand** (Actions → *Throughput Benchmarks* → *Run workflow*, with an optional filter). It has no push or schedule trigger, so it costs no minutes unless started. It uploads `bench-output.txt` and `target/criterion/` as the artifact `bench-<os>-<sha>`.

---

## 2. Results (November 2026, measured under load)

**Machine:** Intel Core i5-13420H (8 cores / 12 threads), Windows 11, Rust 1.98.1, release profile. Shells: `cmd.exe` and Windows PowerShell 5.1 (`pwsh` 7 was not installed).

> **Measured under load: compare ratios, not absolute MB/s.** Nine other MRT builds ran at the same time (40–50 `cargo`/`rustc` processes, 60–100 % CPU load). Absolute numbers are therefore pessimistic, probably by 2–3×. For example, `parse_ansi_text` on plain text measured 59 MiB/s under criterion and 193 MiB/s as best-of-5 in a quieter moment. The **ratios** and the **growth with size** are what matter here. Re-run on an idle machine for clean absolute numbers.

### Parsing and classification (criterion, median of 10 samples)

| Benchmark | Time | Throughput |
| :--- | ---: | ---: |
| `vt_parse/plain_10MB` | 168 ms | 59 MiB/s |
| `vt_parse/colored_10MB` | 394 ms | 25 MiB/s |
| `classify_output/json_10MB` | 658 ms | 15 MiB/s |
| `classify_output/csv_10MB` | 400 ms | 25 MiB/s |
| `classify_output/log_10MB` | 21 ms | 471 MiB/s |
| `classify_output/log_comma_header_10MB` | 19 ms | 529 MiB/s |
| `block_new_completed/colored_10MB` | 373 ms | 27 MiB/s |
| `block_new_completed/json_10MB` | 520 ms | 19 MiB/s |

### GUI streaming re-parse (64 KiB per frame, colored text)

| Output size | Total parse time while streaming | Effective throughput |
| ---: | ---: | ---: |
| 1 MB | 0.32 s | 3.1 MiB/s |
| 2 MB | 1.42 s | 1.4 MiB/s |
| 4 MB | 4.62 s | 0.89 MiB/s |
| 8 MB | 14.3 s | 0.57 MiB/s |

Each doubling of the output costs about 3–4× more time, which is quadratic growth. All of this work runs on the UI thread.

### End-to-end streaming (`execute_command_streaming_with_pid`)

| Shell | Output | Time | Throughput |
| :--- | :--- | ---: | ---: |
| `cmd` | 100 MB plain | 6.1 s | 16 MiB/s |
| `cmd` | 100 MB colored | 4.6 s | 22 MiB/s |
| Windows PowerShell 5.1 | 16 MB plain | 16.0 s | 1.0 MiB/s |
| Windows PowerShell 5.1 | 16 MB colored | 15.2 s | 1.05 MiB/s |

For comparison, scratch probes read the same 100 MB from the same generator through `cmd` in a bare loop. These probes were outside the suite and very noisy on the loaded machine, so treat them as rough:

| Reader | Throughput |
| :--- | ---: |
| `std` blocking read, 8 KiB buffer | 37–437 MiB/s |
| `std` blocking read, 64 KiB buffer | 106–376 MiB/s |
| `tokio` async read, 8 KiB buffer (what `pty.rs` uses) | 54–101 MiB/s |
| `tokio` async read, 64 KiB buffer | 26–278 MiB/s |

### Inside the VT parser (scratch probe, best of 5, 10 MB)

| Variant | Plain | Colored |
| :--- | ---: | ---: |
| `vte` 0.13, no-op `Perform` (parser cost only) | 549 MiB/s | 332 MiB/s |
| `vte` 0.13, push each char into two `String`s | 183 MiB/s | 204 MiB/s |
| MRT `parse_ansi_text` | 193 MiB/s | 97 MiB/s |
| `vte` 0.15 bulk `advance(&[u8])`, no-op | 3106 MiB/s | 453 MiB/s |
| `vte` 0.15 bulk, push each char into two `String`s | 304 MiB/s | 223 MiB/s |

### Rendering a large block in egui (scratch probe, 1400×900 headless `egui::Context`)

This replicates `render_ansi_spans`: one `LayoutJob` for the whole block, shown in a `ScrollArea`. egui is not a dependency of `mrt-core`, so this is not in the criterion suite yet.

| Block | Spans | Build `LayoutJob` | First frame (layout) | Later frames (cached galley) |
| :--- | ---: | ---: | ---: | ---: |
| 1 MB plain | 1 | 0.4 ms | 162 ms | 0.6 ms |
| 4 MB plain | 1 | 0.8 ms | 747 ms | 2.6–34 ms |
| 10 MB plain | 1 | 2.9 ms | **5.1 s** | 3.3–300 ms |
| 1 MB colored | 90 576 | 5.7 ms | 147 ms | 5 ms |
| 4 MB colored | 360 252 | 20 ms | 273 ms | 20–32 ms |
| 10 MB colored | 899 184 | 49 ms | 782 ms | 53–102 ms |

---

## 3. Hot spots

1. **The whole output is re-parsed on every frame while a command streams.** `process_async_events` in [`app.rs`](../crates/mrt-app/src/app.rs) appends the new chunk and then calls `parse_ansi_text(&block.raw_output)` on everything received so far. With a steady chunk size this is O(n²): 8 MB of output costs about 14 s of parsing on the UI thread (table above). A fast producer makes chunks bigger, which reduces the number of frames but not the quadratic shape. **This is the main reason the UI freezes on large output.**
2. **The whole block is laid out as one egui galley, and it changes on every frame while streaming.** `render_ansi_spans` in [`block_view.rs`](../crates/mrt-app/src/widgets/block_view.rs) builds a `LayoutJob` from all spans on every frame (about 50 ms for 10 MB of colored text, so a 20 fps ceiling with nothing else running). Because the text changes while the command runs, egui's galley cache misses and the whole text is laid out again: 0.2–5 s per frame for 1–10 MB. There is no virtualization: off-screen lines cost the same as visible ones. A finished 10 MB plain-text block also costs 5 s on its first frame.
3. **Every block keeps several full copies of its output, without a limit.**
   - Per block: `raw_output`, `plain_output`, and `ansi_spans` (one heap `String` per span; 10 MB of colored text gives about 900 000 spans) hold roughly three copies of the text plus about 40 bytes of overhead per span. The egui galley adds a glyph record (tens of bytes) for every character.
   - While streaming, `execute_command_impl` in [`pty.rs`](../crates/mrt-core/src/pty.rs) also keeps `stdout_raw` and `stderr_raw` byte buffers and a `combined` string. After exit it makes several more full copies (`from_utf8_lossy`, then `.replace("\r\n", "\n")` on stdout, stderr, and the combined output). The GUI keeps its own `raw_output` at the same time.
   - When a command finishes, the `BlockCompleted` handler clones the block three times on the UI thread: for the history worker, the MCP state mirror, and the tab. The history worker stores `plain_output` and the **whole block serialized as JSON, spans included**, in SQLite. `local_server_state.blocks` (the MCP mirror) grows for the whole session. Tabs never drop old blocks either.
4. **Windows PowerShell passes native program output through at about 1 MiB/s.** That is about 20× slower than `cmd` running the same generator. PowerShell decodes native stdout into strings line by line and writes them out again. The limit is in the shell, not in MRT, but PowerShell is the default shell on Windows, so most Windows users hit it with large output. `pwsh` 7 was not measured.
5. **The streaming read loop is several times slower than the pipe.** MRT reaches 16–22 MiB/s through `cmd`, while a bare `tokio` read loop on the same pipe reaches roughly 50–100 MiB/s and a `std` loop 100–400 MiB/s. Part of the gap is the 8 KiB read buffer. The rest is per-read work: `decode_output` allocates a new `String` for every read, the callback copies it, `combined` copies it again, and the final post-processing passes described in item 3 add more.
6. **VT parsing costs about 10 ns per byte.** `vte` 0.13 has only a per-byte `advance`, and MRT pushes every printable character separately into both the span text and `plain_text`, so the text is stored twice. vte's parser alone runs at 550 MiB/s; the per-character pushes bring MRT down to about 190 MiB/s plain and 100 MiB/s colored (idle-machine probe). Colored text is slower because each SGR sequence flushes a span (a new `String` allocation) and clones the attributes.
7. **Classification is slow on large structured output, but it runs once.** `serde_json::Value` parsing of a 10 MB JSON array ran at 15 MiB/s under load, because every object becomes a `BTreeMap` with owned `String` keys. CSV detection allocates a `String` for every cell (25 MiB/s). Plain logs are rejected quickly (about 500 MiB/s). This runs in the Tokio task when the command exits, so it adds latency but does not freeze the UI.

---

## 4. Recommended fixes, ranked by impact

**Status (Milestone 3):** fix 1 is implemented (`VtStream` + `CommandBlock::append_output`). Fix 2 is implemented as tail rendering rather than full virtualization: a block draws its last 2,000 lines (`tail_spans`, cost independent of output size) with "Show more" / "Show all". Fix 3 is implemented for the limit (`terminal.max_block_output_mb`, default 32 MiB, applied while streaming, in `ShellSession` and in `new_completed`); sharing blocks through `Arc` and dropping spans from history are not done yet. Fix 7 is implemented (no structured classification above 4 MiB). Fixes 4–6 are open. The idle UI now repaints once a second instead of four times, and worker / MCP events wake it immediately.

None of these were implemented in the benchmark PR. Items 1–3 touch `app.rs`, `block_view.rs`, `vt.rs`, and `pty.rs`, which other work is changing at the same time, so they should land as their own changes and be checked against these benchmarks.

| # | Fix | Where | Expected impact | Effort |
| :-: | :--- | :--- | :--- | :--- |
| 1 | **Parse output incrementally while streaming.** Keep one `vte::Parser` and `VtParserState` per running block and feed only the new chunk (`VtStream::feed(&str)` that appends to `spans` and `plain_text`). The span being built at the chunk boundary just stays open. Do not parse again when the command finishes; reuse the streamed state. | `vt.rs` (new incremental API), `app.rs::process_async_events`, `CommandBlock::new_completed` | Total parse work drops from O(n²) to O(n). The 8 MB case goes from about 14 s to about 0.3 s of UI-thread time. **Largest win.** | Small–medium |
| 2 | **Virtualize block rendering.** Store line offsets per block (or split spans into lines), render with `ScrollArea::show_rows` so only visible lines are laid out, and cache the `LayoutJob`/galley per block with an invalidation counter. While streaming, re-lay out only the last line. | `block_view.rs`, `app.rs` | Per-frame cost no longer depends on output size; it depends only on visible lines. Removes the 0.2–5 s layouts. | Medium |
| 3 | **Limit and share retained output.** Keep at most N MB per block (for example 16 MB, configurable) as a ring of recent output with an "N MB truncated, open full log" marker. Optionally write the full output to a temporary file. Store blocks as `Arc<CommandBlock>` so the tab, history, and MCP mirror share one copy. Do not serialize `ansi_spans` into history (rebuild them from `raw_output` when loading). Limit the MCP mirror and old tab blocks. | `app.rs`, `history.rs`, `block.rs`, `mrt-mcp` state | Memory becomes bounded (today it grows by an estimated 6–10× the output size per command, counting the copies in hot spot 3; not measured yet). The completion step drops from three deep clones to pointer copies. | Medium |
| 4 | **Faster streaming loop.** Read with 64 KiB buffers. Decode into a reused buffer instead of a new `String` per read. When streaming, do not also build `stdout_raw`/`stderr_raw` strings and the `combined` copy (or build them only on request). Do the `\r\n` replacement once, while streaming. | `pty.rs::execute_command_impl`, `decode_output` | About 2–5× streaming throughput (from about 20 MiB/s towards the 50–100 MiB/s that a bare tokio loop reaches) and about 3 fewer full copies of the output. | Small |
| 5 | **Upgrade `vte` to 0.15 and push printable runs, not single characters.** The bulk `advance(&mut performer, &[u8])` has a fast path for plain ground-state text (6× faster parser in the probe). Keep `plain_text` as the only text copy and store span ranges (`Range<usize>` into `plain_text`) instead of a `String` per span. | `vt.rs`, `block_view.rs` | About 1.5–2× parse throughput, about half the text memory, and far fewer allocations for colored output. Matters most after fix 1, when parsing is no longer quadratic. | Medium (API change) |
| 6 | **Document the PowerShell limit and offer a fast path.** Tell users that large native output under Windows PowerShell is limited to about 1 MiB/s by the shell. Measure `pwsh` 7, and consider running plain native invocations through a direct spawn or `cmd` when the user opts in. | `docs/USER_GUIDE.md`, possibly `pty.rs` | Up to about 20× for heavy-output commands on Windows. | Small (docs), medium (fast path) |
| 7 | **Limit structured classification by size.** Skip JSON/CSV detection above a threshold (for example 4 MB), or classify lazily when the user switches to the structured view. Store CSV rows as offsets instead of a `String` per cell. | `block.rs::classify_output` | Removes 0.4–0.7 s of exit latency per 10 MB of structured output and a large number of allocations. | Small |

### Small fix that was tried and not kept

Two micro-optimizations in `vt.rs` were measured before and after: moving long span text out of the parser instead of cloning it, and reserving `plain_text` capacity up front. Neither changed throughput by a statistically significant amount on this machine (plain: −9 % to +1 % time change, p = 0.10; colored was noisier still). The parser cost is dominated by per-byte `advance` calls and per-character pushes (hot spot 6), not by the final copies, so the change was not kept. Fix 5 is the real version of it.

### Suggested follow-up benchmarks

- An `mrt-app` benchmark for rendering a large block through a headless `egui::Context`, as in the probe above, so fix 2 has a regression gate.
- A peak-memory measurement for a 100 MB command (for example with a counting global allocator in the benchmark), so fix 3 has a number to beat.
- `pwsh` 7, `bash`/`zsh` on macOS, and Linux numbers from the CI workflow artifact.
