//! `mrt share`, `mrt pair`, `mrt attach` and `--host` for the other subcommands:
//! the command-line side of remote connections (`mrt_mcp::remote`).

use anyhow::{anyhow, bail, Context, Result};
use mrt_mcp::remote::{self, PairStore, PairedDevice, ShareHost};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

pub const USAGE: &str = "\
Remote connections (end-to-end encrypted, paired devices only):
  mrt share [--listen ADDR:PORT] [--no-pairing] [--web [--web-listen ADDR:PORT]]
                                          Let paired devices attach to this MRT (default
                                          0.0.0.0:7682); shows a pairing code. --web also
                                          serves the phone companion over HTTPS (:7683)
  mrt share --list | --revoke NAME        Paired devices / remove one (its connections end)
  mrt pair HOST[:PORT] [--name NAME]      Pair this device with a host running `mrt share`
  mrt attach HOST [-t TAB]                Follow a host tab live and type commands into it
  mrt list|send|capture|split --host HOST ...   Run those commands on a paired host";

fn store() -> Result<PairStore> {
    let path = remote::pair_store_path().ok_or_else(|| anyhow!("no data folder"))?;
    PairStore::load(&path)
}

fn this_device_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "device".into())
}

fn with_port(address: &str) -> String {
    if address.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        address.to_string()
    } else {
        format!("{address}:{}", remote::DEFAULT_SHARE_PORT)
    }
}

/// The paired host called `name` (its name or address).
fn find_host(store: &PairStore, name: &str) -> Result<PairedDevice> {
    store
        .hosts
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name) || h.address.as_deref() == Some(with_port(name).as_str()))
        .cloned()
        .ok_or_else(|| anyhow!("no paired host called {name}; run `mrt pair {name}` first"))
}

fn key_of(device: &PairedDevice) -> Result<Vec<u8>> {
    (0..device.key.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&device.key[i..i + 2], 16).map_err(|e| anyhow!(e)))
        .collect()
}

pub async fn connect(name: &str) -> Result<remote::SecureChannel> {
    let store = store()?;
    let host = find_host(&store, name)?;
    let address = host.address.clone().ok_or_else(|| anyhow!("host has no address"))?;
    remote::connect_to_host(&address, &key_of(&host)?, &store).await
}

/// Calls one MCP tool on a paired host: `(text, is_error)`.
pub async fn call_remote(channel: &mut remote::SecureChannel, name: &str, arguments: Value) -> Result<(String, bool)> {
    let request = mrt_mcp::JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": arguments })),
    };
    let response = channel.call(&request).await?;
    if let Some(error) = response.error {
        bail!(error.message);
    }
    let result = response.result.unwrap_or(Value::Null);
    let text = result["content"]
        .as_array()
        .map(|items| items.iter().filter_map(|i| i["text"].as_str()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    Ok((text, result["isError"].as_bool().unwrap_or(false)))
}

fn prompt(question: &str) -> String {
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

async fn share(args: &[String]) -> Result<()> {
    let mut listen = format!("0.0.0.0:{}", remote::DEFAULT_SHARE_PORT);
    let mut pairing = true;
    let mut web: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => {
                i += 1;
                listen = args.get(i).cloned().ok_or_else(|| anyhow!("--listen needs ADDR:PORT"))?;
            }
            "--no-pairing" => pairing = false,
            "--web" => {
                web.get_or_insert_with(|| format!("0.0.0.0:{}", mrt_mcp::companion::DEFAULT_COMPANION_PORT));
            }
            "--web-listen" => {
                i += 1;
                web = Some(args.get(i).cloned().ok_or_else(|| anyhow!("--web-listen needs ADDR:PORT"))?);
            }
            "--list" => {
                let store = store()?;
                if store.guests.is_empty() {
                    println!("No paired devices.");
                }
                for guest in &store.guests {
                    println!("{}\t{}", guest.name, &guest.key[..16]);
                }
                return Ok(());
            }
            "--revoke" => {
                i += 1;
                let name = args.get(i).ok_or_else(|| anyhow!("--revoke needs a device name"))?;
                let mut store = store()?;
                if !store.revoke(name) {
                    bail!("no paired device called {name}");
                }
                store.save()?;
                println!("Revoked {name}. A running `mrt share` drops its connections at their next request.");
                return Ok(());
            }
            other => bail!("unknown option {other} for `mrt share`"),
        }
        i += 1;
    }
    let bridge = mrt_mcp::read_bridge_endpoint().ok_or_else(|| anyhow!("no MRT is running here; start `mrt` or `mrt server` first"))?;
    let store_path = remote::pair_store_path().ok_or_else(|| anyhow!("no data folder"))?;
    let store = Arc::new(Mutex::new(PairStore::load(&store_path)?));
    let code = pairing.then(remote::new_pairing_code);
    let listener = tokio::net::TcpListener::bind(&listen).await.with_context(|| format!("cannot listen on {listen}"))?;
    eprintln!("Sharing this MRT on {listen} (encrypted; paired devices only). Ctrl+C stops sharing.");
    if let Some(code) = &code {
        eprintln!("Pairing code: {code}   On the other device run: mrt pair <this-host>  and type the code.");
    }
    // Revocations made with `mrt share --revoke` in another terminal apply here too.
    let reload = store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if let Ok(fresh) = PairStore::load(&store_path) {
                if let Ok(mut current) = reload.lock() {
                    current.guests = fresh.guests;
                }
            }
        }
    });
    let approve: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|name: &str| {
        let answer = prompt(&format!("Allow device \"{name}\" to attach to this MRT (run commands, read output)? [y/N] "));
        matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes")
    });
    if let Some(web_listen) = web {
        let dir = remote::pair_store_path().and_then(|p| p.parent().map(|d| d.to_path_buf())).ok_or_else(|| anyhow!("no data folder"))?;
        let (cert, key, fingerprint) = mrt_mcp::companion::load_or_create_certificate(&dir)?;
        let web_listener = tokio::net::TcpListener::bind(&web_listen).await.with_context(|| format!("cannot listen on {web_listen}"))?;
        let companion = Arc::new(mrt_mcp::companion::CompanionHost::new(store.clone(), code.clone(), approve.clone(), bridge.clone()));
        let port = web_listener.local_addr()?.port();
        eprintln!("Phone companion: https://<this computer's address>:{port}/  (self-signed certificate; check its SHA-256 fingerprint:");
        eprintln!("  {fingerprint})");
        tokio::spawn(async move {
            if let Err(e) = mrt_mcp::companion::serve_companion(web_listener, &cert, &key, companion).await {
                eprintln!("mrt: companion stopped: {e}");
            }
        });
    }
    let host = Arc::new(ShareHost { store, pairing_code: code, host_name: this_device_name(), approve, bridge });
    tokio::select! {
        _ = remote::serve_share(listener, host) => {}
        _ = tokio::signal::ctrl_c() => eprintln!("Sharing stopped."),
    }
    Ok(())
}

async fn pair(args: &[String]) -> Result<()> {
    let host = args.first().ok_or_else(|| anyhow!("Usage: mrt pair HOST[:PORT] [--name NAME]"))?;
    let name = match args.iter().position(|a| a == "--name") {
        Some(i) => args.get(i + 1).cloned().ok_or_else(|| anyhow!("--name needs a value"))?,
        None => this_device_name(),
    };
    let address = with_port(host);
    let code = prompt(&format!("Pairing code shown by `mrt share` on {host}: "));
    let mut store = store()?;
    eprintln!("Waiting for the host to approve \"{name}\"…");
    let (host_name, key) = remote::pair_with_host(&address, &code, &name, &store).await?;
    let key_hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    store.hosts.retain(|h| h.address.as_deref() != Some(address.as_str()));
    store.hosts.push(PairedDevice { name: host_name.clone(), key: key_hex, address: Some(address.clone()) });
    store.save()?;
    println!("Paired with {host_name} ({address}). Try: mrt attach {host_name}");
    Ok(())
}

/// What `attach` printed of each block, so output is shown once even across reconnects.
#[derive(Default)]
pub struct Follow {
    printed: HashMap<String, (usize, bool)>,
}

impl Follow {
    /// New text from a `mrt_list_blocks` result (blocks newest first).
    pub fn update(&mut self, blocks: &[Value]) -> String {
        let mut out = String::new();
        for block in blocks.iter().rev() {
            let id = block["id"].as_str().unwrap_or_default().to_string();
            let output = block["output"].as_str().unwrap_or_default();
            let finished = !block["exit_code"].is_null();
            let entry = self.printed.entry(id).or_insert_with(|| {
                out.push_str(&format!("$ {}\n", block["command"].as_str().unwrap_or_default()));
                (0, false)
            });
            if entry.1 {
                continue;
            }
            if output.len() > entry.0 && output.is_char_boundary(entry.0) {
                out.push_str(&output[entry.0..]);
                entry.0 = output.len();
                if !output.ends_with('\n') && finished {
                    out.push('\n');
                }
            }
            if finished {
                out.push_str(&format!("[exit {}]\n", block["exit_code"]));
                entry.1 = true;
            }
        }
        out
    }
}

async fn attach(args: &[String]) -> Result<()> {
    let host = args.first().ok_or_else(|| anyhow!("Usage: mrt attach HOST [-t TAB]"))?.clone();
    let tab = match args.iter().position(|a| a == "-t") {
        Some(i) => args.get(i + 1).cloned().unwrap_or_default(),
        None => String::new(),
    };
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    eprintln!("Attached to {host} (tab {}). Type a command and Enter to run it there; Ctrl+C detaches.", if tab.is_empty() { "active" } else { &tab });
    let mut follow = Follow::default();
    let mut channel: Option<remote::SecureChannel> = None;
    loop {
        if channel.is_none() {
            match connect(&host).await {
                Ok(c) => channel = Some(c),
                Err(e) => {
                    eprintln!("[not connected: {e}; retrying]");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            }
        }
        let ch = channel.as_mut().expect("connected");
        while let Ok(line) = line_rx.try_recv() {
            let args = json!({ "tab_id": tab, "input_text": line, "submit_input": true });
            if call_remote(ch, "mrt_control_gui", args).await.is_err() {
                channel = None;
                break;
            }
        }
        let Some(ch) = channel.as_mut() else { continue };
        match call_remote(ch, "mrt_list_blocks", json!({ "tab_id": tab, "limit": 10 })).await {
            Ok((text, _)) => {
                let blocks = serde_json::from_str::<Value>(&text).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let fresh = follow.update(&blocks);
                if !fresh.is_empty() {
                    print!("{fresh}");
                    let _ = std::io::stdout().flush();
                }
            }
            Err(_) => {
                eprintln!("[connection lost; reconnecting]");
                channel = None;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// Runs `mrt share|pair|attach ...`.
pub fn run(sub: &str, args: &[String]) -> i32 {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("mrt: {e}");
            return 70;
        }
    };
    let result = runtime.block_on(async {
        match sub {
            "share" => share(args).await,
            "pair" => pair(args).await,
            "attach" => attach(args).await,
            _ => Err(anyhow!("unknown subcommand")),
        }
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mrt: {e}\n\n{USAGE}");
            69
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reconnect milestone: work continues on the host while the guest is away,
    /// and after reconnecting the guest shows what it missed exactly once, with the
    /// exit status.
    #[test]
    fn reconnecting_shows_missed_output_once() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let bridge = mrt_mcp::start_gui_bridge_listener(mrt_mcp::MrtMcpServerState::default(), 0, false).await.unwrap();
            let (bridge_port, bridge_token) = (bridge.addr.port(), bridge.token.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let host = Arc::new(ShareHost {
                store: Arc::new(Mutex::new(PairStore::ephemeral().unwrap())),
                pairing_code: Some("424242".into()),
                host_name: "build-box".into(),
                approve: Arc::new(|_| true),
                bridge: (bridge_port, bridge_token.clone()),
            });
            tokio::spawn(remote::serve_share(listener, host));
            let guest = PairStore::ephemeral().unwrap();
            let (_, host_key) = remote::pair_with_host(&address, "424242", "laptop", &guest).await.unwrap();

            let mut follow = Follow::default();
            let mut channel = remote::connect_to_host(&address, &host_key, &guest).await.unwrap();
            let list = |text: String| serde_json::from_str::<Value>(&text).unwrap().as_array().cloned().unwrap_or_default();
            let (text, _) = call_remote(&mut channel, "mrt_list_blocks", json!({ "tab_id": "", "limit": 10 })).await.unwrap();
            assert_eq!(follow.update(&list(text)), "");
            drop(channel); // the laptop goes away

            // Work goes on at the host meanwhile (a local client of its bridge).
            let request = mrt_mcp::JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: Some(json!(1)),
                method: "tools/call".into(),
                params: Some(json!({ "name": "mrt_run_command", "arguments": { "command": "echo built-while-away" } })),
            };
            mrt_mcp::try_forward_jsonrpc_to_gui(bridge_port, &bridge_token, &request).await.unwrap();

            let mut channel = remote::connect_to_host(&address, &host_key, &guest).await.unwrap();
            let (text, _) = call_remote(&mut channel, "mrt_list_blocks", json!({ "tab_id": "", "limit": 10 })).await.unwrap();
            let shown = follow.update(&list(text));
            assert!(shown.contains("$ echo built-while-away") && shown.contains("built-while-away\n") && shown.contains("[exit 0]"), "{shown}");
            let (text, _) = call_remote(&mut channel, "mrt_list_blocks", json!({ "tab_id": "", "limit": 10 })).await.unwrap();
            assert_eq!(follow.update(&list(text)), "", "shown once");
        });
    }

    #[test]
    fn follow_prints_each_output_once() {
        let mut follow = Follow::default();
        let running = json!([{ "id": "a", "command": "make", "output": "step 1\n", "exit_code": null }]);
        assert_eq!(follow.update(running.as_array().unwrap()), "$ make\nstep 1\n");
        assert_eq!(follow.update(running.as_array().unwrap()), "", "nothing new");
        let done = json!([
            { "id": "b", "command": "ls", "output": "x", "exit_code": 0 },
            { "id": "a", "command": "make", "output": "step 1\nstep 2\n", "exit_code": 0 }
        ]);
        assert_eq!(follow.update(done.as_array().unwrap()), "step 2\n[exit 0]\n$ ls\nx\n[exit 0]\n");
        assert_eq!(follow.update(done.as_array().unwrap()), "");
        assert_eq!(with_port("box"), format!("box:{}", remote::DEFAULT_SHARE_PORT));
        assert_eq!(with_port("10.0.0.2:9000"), "10.0.0.2:9000");
    }
}
