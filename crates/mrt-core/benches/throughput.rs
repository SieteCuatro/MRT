//! Throughput benchmarks for MRT's output pipeline.
//!
//! Output from a command flows through `execute_command_streaming_with_pid` (pipe read +
//! UTF-8 decode), then `vt::parse_ansi_text` (ANSI → spans + plain text), then
//! `block::classify_output` (JSON / CSV / Markdown detection). These benchmarks measure each
//! stage in MB/s on large inputs, plus a simulation of the GUI's per-frame re-parse while a
//! command is still streaming. See `docs/PERFORMANCE.md` for results and analysis.
//!
//! Run with `cargo bench -p mrt-core --bench throughput`. Filter a group with e.g.
//! `cargo bench -p mrt-core --bench throughput -- vt_parse`.
//!
//! The streaming benchmark needs a fast, cross-platform output generator. Shell loops
//! (`yes`, `for` in PowerShell) are slower than MRT itself, so they would measure the
//! generator. Instead this binary re-invokes itself through the shell with
//! `--mrt-emit <bytes> [plain|color]` and writes the bytes to stdout as fast as it can.

use criterion::{BenchmarkId, Criterion, Throughput};
use mrt_core::{
    classify_output, discover_shells, execute_command_streaming_with_pid, parse_ansi_text,
    CommandBlock, ShellKind, ShellProfile,
};
use std::hint::black_box;
use std::io::Write;
use std::time::Duration;

const MB: usize = 1024 * 1024;
const EMIT_FLAG: &str = "--mrt-emit";

// ---------------------------------------------------------------------------------------------
// Deterministic input generators (no RNG dependency, identical on every machine).
// ---------------------------------------------------------------------------------------------

/// A log-like line, ~90 bytes, varying with `i`.
fn plain_line(i: usize, out: &mut String) {
    use std::fmt::Write as _;
    let level = ["INFO", "WARN", "DEBUG", "ERROR"][i % 4];
    let _ = writeln!(
        out,
        "2026-11-16T12:{:02}:{:02}.{:03}Z {level:<5} worker-{} processed request id={} status=ok",
        (i / 3600) % 60,
        (i / 60) % 60,
        i % 1000,
        i % 16,
        i
    );
}

/// The same line with an SGR color change on almost every token, like `ls --color`,
/// `cargo` diagnostics or `git log --color` produce. Roughly half of the bytes are escapes.
fn colored_line(i: usize, out: &mut String) {
    use std::fmt::Write as _;
    let color = 31 + (i % 7);
    let _ = writeln!(
        out,
        "\x1b[2m2026-11-16T12:{:02}:{:02}\x1b[0m \x1b[1;{color}m{:<5}\x1b[0m \x1b[36mworker-{}\x1b[0m \
         processed \x1b[38;2;{};{};{}mrequest\x1b[0m id=\x1b[33m{}\x1b[0m status=\x1b[32mok\x1b[0m",
        (i / 60) % 60,
        i % 60,
        ["INFO", "WARN", "DEBUG", "ERROR"][i % 4],
        i % 16,
        i % 256,
        (i * 7) % 256,
        (i * 13) % 256,
        i
    );
}

fn build_text(target_bytes: usize, line: fn(usize, &mut String)) -> String {
    let mut out = String::with_capacity(target_bytes + 256);
    let mut i = 0;
    while out.len() < target_bytes {
        line(i, &mut out);
        i += 1;
    }
    out
}

fn build_json(target_bytes: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(target_bytes + 256);
    out.push_str("[\n");
    let mut i = 0;
    while out.len() < target_bytes {
        if i > 0 {
            out.push_str(",\n");
        }
        let _ = write!(
            out,
            r#"  {{"id": {i}, "name": "item-{i}", "active": {}, "score": {}.5, "tags": ["alpha", "beta", "gamma"], "owner": {{"login": "user{}", "uid": {}}}}}"#,
            i % 2 == 0,
            i % 1000,
            i % 97,
            1000 + i % 97
        );
        i += 1;
    }
    out.push_str("\n]\n");
    out
}

fn build_csv(target_bytes: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(target_bytes + 256);
    out.push_str("pid,name,cpu,memory_kb,user,state,started\n");
    let mut i = 0;
    while out.len() < target_bytes {
        let _ = writeln!(
            out,
            "{},proc-{},{}.{},{},user{},running,2026-11-16T12:00:{:02}",
            1000 + i,
            i % 500,
            i % 100,
            i % 10,
            4096 + (i * 37) % 900_000,
            i % 12,
            i % 60
        );
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Generator mode: `throughput --mrt-emit <bytes> [plain|color]`.
// ---------------------------------------------------------------------------------------------

fn emit(args: &[String]) {
    let bytes: usize = args
        .first()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100 * MB);
    let colored = args.get(1).map(|s| s == "color").unwrap_or(false);
    // Pre-build one 1 MiB chunk and repeat it so the generator costs almost nothing.
    let chunk = build_text(MB, if colored { colored_line } else { plain_line });
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::with_capacity(256 * 1024, stdout.lock());
    let mut remaining = bytes;
    while remaining > 0 {
        let n = remaining.min(chunk.len());
        if out.write_all(&chunk.as_bytes()[..n]).is_err() {
            return;
        }
        remaining -= n;
    }
    let _ = out.flush();
}

// ---------------------------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------------------------

fn bench_vt_parse(c: &mut Criterion) {
    let plain = build_text(10 * MB, plain_line);
    let colored = build_text(10 * MB, colored_line);
    let mut group = c.benchmark_group("vt_parse");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for (name, input) in [("plain_10MB", &plain), ("colored_10MB", &colored)] {
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), input, |b, input| {
            b.iter(|| black_box(parse_ansi_text(black_box(input))))
        });
    }
    group.finish();
}

fn bench_classify(c: &mut Criterion) {
    let json = build_json(10 * MB);
    let csv = build_csv(10 * MB);
    let log = build_text(10 * MB, plain_line);
    // A log whose first line happens to contain a comma: the table detector splits every
    // line before the column count mismatch on line 2 rejects it.
    let mut log_with_comma = String::from("starting, please wait\n");
    log_with_comma.push_str(&log);
    let mut group = c.benchmark_group("classify_output");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for (name, input) in [
        ("json_10MB", &json),
        ("csv_10MB", &csv),
        ("log_10MB", &log),
        ("log_comma_header_10MB", &log_with_comma),
    ] {
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), input, |b, input| {
            b.iter(|| black_box(classify_output(black_box(input))))
        });
    }
    group.finish();
}

/// `CommandBlock::new_completed` is what runs when a command finishes: parse + classify +
/// moving the strings into the block.
fn bench_block_completed(c: &mut Criterion) {
    let colored = build_text(10 * MB, colored_line);
    let json = build_json(10 * MB);
    let mut group = c.benchmark_group("block_new_completed");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for (name, input) in [("colored_10MB", &colored), ("json_10MB", &json)] {
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), input, |b, input| {
            b.iter_batched(
                || input.clone(),
                |raw| black_box(CommandBlock::new_completed("bench", "sh", ".", raw, 0, 0)),
                criterion::BatchSize::LargeInput,
            )
        });
    }
    group.finish();
}

/// Simulates the GUI's `process_async_events` while a command streams: every frame the new
/// chunk is appended to `raw_output` and the *whole* output is parsed again. With a fixed
/// chunk per frame the total work grows with the square of the output size.
fn bench_streaming_reparse(c: &mut Criterion) {
    const CHUNK: usize = 64 * 1024;
    let mut group = c.benchmark_group("gui_reparse_per_frame_64KiB");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for total_mb in [1usize, 2, 4, 8] {
        let input = build_text(total_mb * MB, colored_line);
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{total_mb}MB")),
            &input,
            |b, input| {
                b.iter(|| {
                    let mut raw = String::new();
                    let mut last = (Vec::new(), String::new());
                    let bytes = input.as_bytes();
                    let mut start = 0;
                    while start < bytes.len() {
                        let mut end = (start + CHUNK).min(bytes.len());
                        while !input.is_char_boundary(end) {
                            end += 1;
                        }
                        raw.push_str(&input[start..end]);
                        last = parse_ansi_text(&raw);
                        start = end;
                    }
                    black_box(last)
                })
            },
        );
    }
    group.finish();
}

/// Builds the shell command that runs this benchmark binary as the output generator.
fn generator_command(shell: &ShellProfile, bytes: usize, mode: &str) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_str()?.to_string();
    match shell.kind {
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell => Some(format!(
            "& '{}' {EMIT_FLAG} {bytes} {mode}",
            exe.replace('\'', "''")
        )),
        // Rust escapes embedded quotes as `\"`, which cmd.exe does not understand, so only
        // paths without spaces (which need no quotes) are supported here.
        ShellKind::CmdBatch if !exe.contains(' ') => {
            Some(format!("{exe} {EMIT_FLAG} {bytes} {mode}"))
        }
        ShellKind::CmdBatch => None,
        ShellKind::Sh | ShellKind::Bash | ShellKind::Zsh => Some(format!(
            "'{}' {EMIT_FLAG} {bytes} {mode}",
            exe.replace('\'', r"'\''")
        )),
        _ => None,
    }
}

fn bench_streaming(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let cwd = std::env::current_dir().expect("cwd");
    // Raw pipe throughput: a shell that hands its stdout straight to the generator.
    // PowerShell is measured separately because it re-encodes native output line by line,
    // which is what Windows users get by default.
    let wanted: &[(ShellKind, usize)] = if cfg!(windows) {
        &[
            (ShellKind::CmdBatch, 100 * MB),
            (ShellKind::PowerShellCore, 16 * MB),
            (ShellKind::WindowsPowerShell, 16 * MB),
        ]
    } else {
        &[(ShellKind::Sh, 100 * MB), (ShellKind::Bash, 100 * MB)]
    };
    let shells = discover_shells();
    let mut group = c.benchmark_group("streaming");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    for (kind, bytes) in wanted {
        let Some(shell) = shells.iter().find(|s| &s.kind == kind) else {
            eprintln!("streaming: {kind:?} not installed, skipping");
            continue;
        };
        for mode in ["plain", "color"] {
            let Some(command) = generator_command(shell, *bytes, mode) else {
                eprintln!("streaming: cannot build a generator command for {kind:?}, skipping");
                continue;
            };
            group.throughput(Throughput::Bytes(*bytes as u64));
            let id = BenchmarkId::new(shell.id.clone(), format!("{mode}_{}MB", bytes / MB));
            group.bench_function(id, |b| {
                b.iter(|| {
                    let mut received = 0usize;
                    let result = runtime
                        .block_on(execute_command_streaming_with_pid(
                            shell,
                            &command,
                            &cwd,
                            |_pid| {},
                            |chunk| received += chunk.len(),
                        ))
                        .expect("generator command failed");
                    assert_eq!(result.exit_code, 0, "generator exited with an error");
                    // PowerShell may add or translate line endings; it must not lose data.
                    assert!(
                        received >= *bytes * 9 / 10,
                        "received only {received} of {bytes} bytes"
                    );
                    black_box(result)
                })
            });
        }
    }
    group.finish();
}

criterion::criterion_group!(
    benches,
    bench_vt_parse,
    bench_classify,
    bench_block_completed,
    bench_streaming_reparse,
    bench_streaming
);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(EMIT_FLAG) {
        emit(&args[1..]);
        return;
    }
    benches();
    Criterion::default().configure_from_args().final_summary();
}
