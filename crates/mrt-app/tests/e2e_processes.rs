//! End-to-end tests with real `mrt` processes (no window): `mrt server`, `mrt share`,
//! `mrt pair`, `mrt list|send --host`, revocation, and `mrt session-host`. Every
//! process gets its own `MRT_DATA_DIR`, so nothing touches the user's files.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn mrt() -> &'static str {
    env!("CARGO_BIN_EXE_mrt")
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mrt-e2e-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Kills the processes, then removes the test's data folders.
struct Kill(Vec<Child>, Vec<PathBuf>);
impl Drop for Kill {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
        for dir in &self.1 {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} was not written",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run(data: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(mrt())
        .args(args)
        .env("MRT_DATA_DIR", data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn share_pair_send_and_revoke_between_two_data_folders() {
    let (host_data, guest_data, intruder_data) =
        (temp_dir("host"), temp_dir("guest"), temp_dir("intruder"));
    let mut procs = Kill(
        Vec::new(),
        vec![host_data.clone(), guest_data.clone(), intruder_data.clone()],
    );
    procs.0.push(
        Command::new(mrt())
            .arg("server")
            .env("MRT_DATA_DIR", &host_data)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for_file(&host_data.join("mcp-bridge.json"));

    let port = 20_000 + (std::process::id() % 20_000) as u16;
    let listen = format!("127.0.0.1:{port}");
    let mut share = Command::new(mrt())
        .args(["share", "--listen", &listen])
        .env("MRT_DATA_DIR", &host_data)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut share_in = share.stdin.take().unwrap();
    let stderr = share.stderr.take().unwrap();
    procs.0.push(share);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let code = loop {
        let line = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("pairing code");
        if let Some(rest) = line.split("Pairing code: ").nth(1) {
            break rest.split_whitespace().next().unwrap().to_string();
        }
    };
    share_in.write_all(b"y\n").unwrap();
    share_in.flush().unwrap();

    let (status, out, err) = run(
        &guest_data,
        &["pair", &listen, "--name", "e2e-laptop"],
        &format!("{code}\n"),
    );
    assert_eq!(status, 0, "{out}{err}");
    let host_name = out
        .split("Paired with ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    let (status, out, err) = run(
        &guest_data,
        &["send", "--host", &host_name, "echo", "through-the-relay"],
        "",
    );
    assert_eq!(status, 0, "{err}");
    assert!(out.contains("through-the-relay"), "{out}");
    let (status, out, _) = run(&guest_data, &["list", "--host", &host_name, "--json"], "");
    assert_eq!(status, 0);
    assert!(out.trim_start().starts_with('['), "{out}");

    let (status, _, _) = run(
        &intruder_data,
        &["pair", &listen, "--name", "intruder"],
        "000000\n",
    );
    assert_ne!(status, 0, "a wrong code is refused");

    let (status, _, _) = run(&host_data, &["share", "--revoke", "e2e-laptop"], "");
    assert_eq!(status, 0);
    std::thread::sleep(Duration::from_secs(3)); // the running share reloads paired devices every 2 s
    let (status, _, err) = run(&guest_data, &["list", "--host", &host_name], "");
    assert_ne!(status, 0);
    assert!(err.contains("does not accept this device"), "{err}");
}

#[test]
fn session_host_process_keeps_jobs_for_the_next_window() {
    let data = temp_dir("session-host");
    let mut procs = Kill(Vec::new(), vec![data.clone()]);
    procs.0.push(
        Command::new(mrt())
            .arg("session-host")
            .env("MRT_DATA_DIR", &data)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let file = data.join("session-host.json");
    wait_for_file(&file);
    std::thread::sleep(Duration::from_millis(200));
    let host: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let (port, token) = (
        host["port"].as_u64().unwrap() as u16,
        host["token"].as_str().unwrap().to_string(),
    );
    let call = |request: serde_json::Value| -> serde_json::Value {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        writeln!(stream, "{}", serde_json::json!({ "mrt_host_auth": token })).unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let shells = mrt_core::discover_shells();
    let shell = shells
        .iter()
        .find(|s| {
            matches!(
                s.kind,
                mrt_core::ShellKind::CmdBatch | mrt_core::ShellKind::Bash | mrt_core::ShellKind::Sh
            )
        })
        .unwrap_or(&shells[0]);
    let (tab, block) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    // "Window 1" starts a command and disappears.
    let reply = call(
        serde_json::json!({ "op": "run", "tab_id": tab, "block_id": block, "shell": shell, "command": "echo from-the-host-process", "cwd": std::env::temp_dir() }),
    );
    assert!(reply["ok"].is_object(), "{reply}");
    // "Window 2" finds the job and its output.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let reply = call(serde_json::json!({ "op": "poll", "block_id": block, "from": 0 }));
        if reply["ok"]["done"].is_object() {
            assert!(
                reply["ok"]["output"]
                    .as_str()
                    .unwrap()
                    .contains("from-the-host-process"),
                "{reply}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "job did not finish: {reply}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let jobs = call(serde_json::json!({ "op": "jobs" }));
    assert!(jobs["ok"]
        .as_array()
        .unwrap()
        .iter()
        .any(|j| j["tab_id"] == serde_json::json!(tab)));
    assert_eq!(
        call(serde_json::json!({ "op": "shutdown" }))["ok"]["ok"],
        true
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!file.exists(), "the host removes its file on shutdown");
}
