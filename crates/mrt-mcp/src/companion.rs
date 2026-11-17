//! The mobile companion: a small web app that `mrt share --web` serves over HTTPS on
//! the local network, so a phone can follow tabs, read blocks, send commands, and
//! (when `safety.remote_approvals` is on) answer Safety Gate prompts.
//!
//! - TLS with a self-signed certificate created on first use and kept in the data
//!   folder; `mrt share` prints its SHA-256 fingerprint so the user can check it
//!   in the browser's certificate details.
//! - A device pairs with the same 6-digit code as `mrt pair`, and the host user
//!   approves it by name. It then gets a random token; only the token's SHA-256 is
//!   stored, as a paired device named `web: <name>` (revoked with `mrt share
//!   --revoke`). Five wrong codes close pairing until `mrt share` restarts.
//! - The API exposes only a few tools (tabs, blocks, typing into a tab, switching
//!   tabs, answering the Safety Gate), forwarded to the local bridge.

use crate::remote::{PairStore, PairedDevice};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// The default port of the web companion.
pub const DEFAULT_COMPANION_PORT: u16 = 7683;
const MAX_BODY: usize = 64 * 1024;
const MAX_FAILED_PAIRINGS: u32 = 5;

/// Tools the companion may call.
pub const COMPANION_TOOLS: &[&str] = &["mrt_list_tabs", "mrt_list_blocks", "mrt_get_session_state", "mrt_switch_tab", "mrt_control_gui"];

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Loads or creates the companion's certificate; returns (cert PEM, key PEM, fingerprint).
pub fn load_or_create_certificate(dir: &Path) -> Result<(String, String, String)> {
    let (cert_path, key_path) = (dir.join("companion-cert.pem"), dir.join("companion-key.pem"));
    let (cert, key) = match (std::fs::read_to_string(&cert_path), std::fs::read_to_string(&key_path)) {
        (Ok(cert), Ok(key)) => (cert, key),
        _ => {
            let names = vec!["localhost".to_string(), "mrt.local".to_string()];
            let generated = rcgen::generate_simple_self_signed(names).context("cannot create a certificate")?;
            let (cert, key) = (generated.cert.pem(), generated.key_pair.serialize_pem());
            std::fs::create_dir_all(dir)?;
            std::fs::write(&cert_path, &cert)?;
            std::fs::write(&key_path, &key)?;
            (cert, key)
        }
    };
    let der = pem_body(&cert).ok_or_else(|| anyhow!("certificate file is not PEM"))?;
    let fingerprint = sha256_hex(&der)
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap_or("").to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(":");
    Ok((cert, key, fingerprint))
}

fn pem_body(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in body.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn tls_acceptor(cert_pem: &str, key_pem: &str) -> Result<tokio_rustls::TlsAcceptor> {
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let cert = CertificateDer::from(pem_body(cert_pem).ok_or_else(|| anyhow!("bad certificate"))?);
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pem_body(key_pem).ok_or_else(|| anyhow!("bad key"))?));
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// What the companion server needs.
pub struct CompanionHost {
    pub store: Arc<Mutex<PairStore>>,
    pub pairing_code: Option<String>,
    pub approve: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub bridge: (u16, String),
    failed_pairings: AtomicU32,
}

impl CompanionHost {
    pub fn new(store: Arc<Mutex<PairStore>>, pairing_code: Option<String>, approve: Arc<dyn Fn(&str) -> bool + Send + Sync>, bridge: (u16, String)) -> Self {
        Self { store, pairing_code, approve, bridge, failed_pairings: AtomicU32::new(0) }
    }

    fn token_device(&self, token: &str) -> Option<String> {
        let key = format!("web:{}", sha256_hex(token.as_bytes()));
        let store = self.store.lock().ok()?;
        store.guests.iter().find(|g| g.key == key).map(|g| g.name.clone())
    }

    /// Handles one API request: `(status, JSON body)`.
    pub async fn api(&self, path: &str, token: Option<&str>, body: &[u8]) -> (u16, Value) {
        let body: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        match path {
            "/api/pair" => self.pair(&body).await,
            "/api/call" => {
                let Some(device) = token.and_then(|t| self.token_device(t)) else {
                    return (401, json!({ "error": "not paired (or revoked)" }));
                };
                let tool = body["tool"].as_str().unwrap_or_default();
                if !COMPANION_TOOLS.contains(&tool) {
                    return (403, json!({ "error": format!("{tool} is not available to the companion") }));
                }
                let mut arguments = body["arguments"].clone();
                if !arguments.is_object() {
                    arguments = json!({});
                }
                if tool == "mrt_control_gui" {
                    // Only typing into a tab and answering the Safety Gate.
                    let keep = ["tab_id", "input_text", "submit_input", "safety"];
                    arguments.as_object_mut().unwrap().retain(|k, _| keep.contains(&k.as_str()));
                    arguments["device"] = json!(device);
                }
                let request = crate::protocol::JsonRpcRequest {
                    jsonrpc: "2.0".into(),
                    id: Some(json!(1)),
                    method: "tools/call".into(),
                    params: Some(json!({ "name": tool, "arguments": arguments })),
                };
                match crate::bridge::try_forward_jsonrpc_to_gui(self.bridge.0, &self.bridge.1, &request).await {
                    Ok(Some(response)) => {
                        let result = response.result.unwrap_or(Value::Null);
                        let text = result["content"][0]["text"].as_str().unwrap_or_default().to_string();
                        let data = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
                        (200, json!({ "result": data, "is_error": result["isError"] }))
                    }
                    _ => (502, json!({ "error": "MRT on this computer is not reachable" })),
                }
            }
            _ => (404, json!({ "error": "not found" })),
        }
    }

    async fn pair(&self, body: &Value) -> (u16, Value) {
        let Some(code) = &self.pairing_code else { return (403, json!({ "error": "pairing is closed" })) };
        if self.failed_pairings.load(Ordering::Relaxed) >= MAX_FAILED_PAIRINGS {
            return (429, json!({ "error": "too many wrong codes; restart `mrt share` on the computer" }));
        }
        let given = body["code"].as_str().unwrap_or_default();
        if given.len() != code.len() || !given.bytes().zip(code.bytes()).fold(true, |ok, (a, b)| ok & (a == b)) {
            self.failed_pairings.fetch_add(1, Ordering::Relaxed);
            return (403, json!({ "error": "wrong code" }));
        }
        let name: String = body["name"].as_str().unwrap_or("phone").chars().filter(|c| !c.is_control()).take(40).collect();
        let name = format!("web: {}", if name.trim().is_empty() { "phone" } else { name.trim() });
        let approve = self.approve.clone();
        let asked = name.clone();
        if !tokio::task::spawn_blocking(move || approve(&asked)).await.unwrap_or(false) {
            return (403, json!({ "error": "the computer's user did not approve this device" }));
        }
        let mut raw = [0u8; 32];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut raw);
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let Ok(mut store) = self.store.lock() else { return (500, json!({ "error": "store" })) };
        store.guests.push(PairedDevice { name: name.clone(), key: format!("web:{}", sha256_hex(token.as_bytes())), address: None });
        let _ = store.save_guests();
        (200, json!({ "token": token, "device": name }))
    }
}

/// One HTTP request (enough for the companion's own page and API).
struct Request {
    method: String,
    path: String,
    token: Option<String>,
    body: Vec<u8>,
}

async fn read_request<S: tokio::io::AsyncRead + Unpin>(stream: S) -> Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let (mut length, mut token) = (0usize, None);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "authorization" => token = value.trim().strip_prefix("Bearer ").map(str::to_string),
                _ => {}
            }
        }
    }
    if length > MAX_BODY {
        bail!("body too large");
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Ok(Request { method, path, token, body })
}

fn response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Error",
    };
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// Serves the companion over TLS on `listener` until the task is dropped.
pub async fn serve_companion(listener: TcpListener, cert_pem: &str, key_pem: &str, host: Arc<CompanionHost>) -> Result<()> {
    let acceptor = tls_acceptor(cert_pem, key_pem)?;
    while let Ok((tcp, _)) = listener.accept().await {
        let (acceptor, host) = (acceptor.clone(), host.clone());
        tokio::spawn(async move {
            let Ok(mut tls) = acceptor.accept(tcp).await else { return };
            let reply = match read_request(&mut tls).await {
                Ok(req) if req.method == "GET" && (req.path == "/" || req.path.starts_with("/?")) => {
                    response(200, "text/html; charset=utf-8", COMPANION_PAGE.as_bytes())
                }
                Ok(req) if req.method == "POST" => {
                    let (status, body) = host.api(&req.path, req.token.as_deref(), &req.body).await;
                    response(status, "application/json", body.to_string().as_bytes())
                }
                Ok(_) => response(404, "text/plain", b"not found"),
                Err(_) => return,
            };
            let _ = tls.write_all(&reply).await;
            let _ = tls.shutdown().await;
        });
    }
    Ok(())
}

/// The companion page: tabs with activity, the selected tab's blocks, a command box
/// and the pending Safety Gate prompt.
pub const COMPANION_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>MRT</title>
<style>
body{margin:0;font:15px system-ui,sans-serif;background:#17171a;color:#ececee}
header{padding:12px 14px;background:#111113;font-weight:600}
#tabs{display:flex;gap:6px;overflow-x:auto;padding:8px}
.tab{padding:6px 10px;border-radius:10px;background:#1e1e22;white-space:nowrap;border:1px solid #2e2e34}
.tab.sel{border-color:#8ea2ff}.dot{display:inline-block;width:8px;height:8px;border-radius:4px;margin-right:6px;background:#8a8a93}
.running .dot,.progress .dot{background:#8ea2ff}.failed .dot{background:#f87171}.succeeded .dot{background:#4ade80}.waiting_for_input .dot{background:#fbbf24}
#blocks{padding:0 8px 90px}.blk{background:#1b1b1f;border:1px solid #26262b;border-radius:12px;margin:8px 0;padding:8px}
.cmd{font-family:ui-monospace,monospace;color:#b8c2ff}.out{font:12px ui-monospace,monospace;white-space:pre-wrap;color:#c4c4cc;max-height:40vh;overflow:auto}
.fail{border-color:#5a3a22}#bar{position:fixed;bottom:0;left:0;right:0;display:flex;gap:6px;padding:10px;background:#111113}
input{flex:1;padding:10px;border-radius:10px;border:1px solid #34343b;background:#1e1e22;color:#ececee;font:15px ui-monospace,monospace}
button{padding:10px 14px;border-radius:10px;border:0;background:#8ea2ff;color:#0f1222;font-weight:600}
#gate{display:none;margin:8px;padding:10px;border-radius:12px;background:#221d1a;border:1px solid #5a3a22}
#pair{padding:16px}.muted{color:#8a8a93}
</style></head><body>
<header>MRT companion <span id="status" class="muted"></span></header>
<div id="pair" style="display:none"><p>Enter the pairing code shown by <code>mrt share</code> on your computer.</p>
<p><input id="code" inputmode="numeric" maxlength="6" placeholder="123456"></p>
<p><input id="name" placeholder="Device name (e.g. my phone)"></p><p><button onclick="pair()">Pair</button></p><p id="perr" class="muted"></p></div>
<div id="main" style="display:none"><div id="tabs"></div><div id="gate"></div><div id="blocks"></div>
<div id="bar"><input id="line" placeholder="Command for this tab" autocapitalize="off" autocorrect="off"><button onclick="send()">Run</button></div></div>
<script>
let token=localStorage.getItem('mrt_token'), tab=null;
const $=id=>document.getElementById(id);
async function call(tool,args){const r=await fetch('/api/call',{method:'POST',headers:{'Authorization':'Bearer '+token,'Content-Type':'application/json'},body:JSON.stringify({tool,arguments:args||{}})});
 if(r.status===401){localStorage.removeItem('mrt_token');token=null;show();throw 'unpaired'} return (await r.json()).result}
async function pair(){const r=await fetch('/api/pair',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({code:$('code').value,name:$('name').value})});
 const j=await r.json(); if(j.token){token=j.token;localStorage.setItem('mrt_token',token);show()} else $('perr').textContent=j.error}
function esc(s){return String(s??'').replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]))}
async function refresh(){if(!token)return;try{
 const st=await call('mrt_get_session_state'); const tabs=st.tabs||[]; if(!tab||!tabs.some(t=>t.id===tab))tab=(tabs.find(t=>t.active)||tabs[0]||{}).id;
 $('tabs').innerHTML=tabs.map(t=>`<div class="tab ${t.id===tab?'sel':''} ${esc(t.activity&&t.activity.state)}" onclick="tab='${t.id}';refresh()"><span class="dot"></span>${esc(t.title)}</div>`).join('');
 const g=st.pending_approval; $('gate').style.display=g?'block':'none';
 if(g)$('gate').innerHTML=`<b>Safety Gate: ${esc(g.risk)}</b><div class="cmd">${esc(g.command)}</div><div class="muted">${esc(g.reason)}</div><p><button onclick="decide('approve')">Approve</button> <button style="background:#f87171" onclick="decide('deny')">Deny</button></p><div class="muted">Works when safety.remote_approvals is on.</div>`;
 const blocks=await call('mrt_list_blocks',{tab_id:tab,limit:15});
 $('blocks').innerHTML=(blocks||[]).slice().reverse().map(b=>`<div class="blk ${b.exit_code&&b.exit_code!==0?'fail':''}"><div class="cmd">$ ${esc(b.command)} <span class="muted">${b.exit_code===null?'running…':'exit '+b.exit_code}</span></div><div class="out">${esc((b.output||'').slice(-4000))}</div></div>`).join('');
 $('status').textContent='· connected'}catch(e){$('status').textContent='· offline'}}
async function send(){const v=$('line').value.trim(); if(!v)return; $('line').value=''; await call('mrt_control_gui',{tab_id:tab,input_text:v,submit_input:true}); setTimeout(refresh,400)}
async function decide(d){await call('mrt_control_gui',{safety:d}); setTimeout(refresh,400)}
function show(){$('pair').style.display=token?'none':'block';$('main').style.display=token?'block':'none'; refresh()}
show(); setInterval(refresh,1500);
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    async fn companion(approve: bool) -> Arc<CompanionHost> {
        let bridge = crate::bridge::start_gui_bridge_listener(crate::server::MrtMcpServerState::default(), 0, false).await.unwrap();
        Arc::new(CompanionHost::new(
            Arc::new(Mutex::new(PairStore::ephemeral().unwrap())),
            Some("314159".into()),
            Arc::new(move |_| approve),
            (bridge.addr.port(), bridge.token),
        ))
    }

    #[tokio::test]
    async fn pairing_tokens_and_tool_limits() {
        let host = companion(true).await;
        let (status, _) = host.api("/api/call", Some("nope"), br#"{"tool":"mrt_list_tabs"}"#).await;
        assert_eq!(status, 401);
        let (status, _) = host.api("/api/pair", None, br#"{"code":"000000","name":"phone"}"#).await;
        assert_eq!(status, 403);
        let (status, body) = host.api("/api/pair", None, br#"{"code":"314159","name":"Ana's phone"}"#).await;
        assert_eq!(status, 200, "{body}");
        let token = body["token"].as_str().unwrap().to_string();
        assert!(!host.store.lock().unwrap().guests[0].key.contains(&token), "only the token's hash is stored");

        let (status, body) = host.api("/api/call", Some(&token), br#"{"tool":"mrt_get_session_state"}"#).await;
        assert_eq!(status, 200, "{body}");
        assert!(body["result"]["tabs"].is_array());
        let (status, _) = host.api("/api/call", Some(&token), br#"{"tool":"mrt_run_command","arguments":{"command":"whoami"}}"#).await;
        assert_eq!(status, 403, "only the companion's tools");

        host.store.lock().unwrap().revoke("web: Ana's phone");
        let (status, _) = host.api("/api/call", Some(&token), br#"{"tool":"mrt_list_tabs"}"#).await;
        assert_eq!(status, 401, "revoked");
    }

    #[tokio::test]
    async fn wrong_codes_close_pairing_and_approval_is_required() {
        let host = companion(false).await;
        let (status, _) = host.api("/api/pair", None, br#"{"code":"314159"}"#).await;
        assert_eq!(status, 403, "not approved");
        for _ in 0..MAX_FAILED_PAIRINGS {
            host.api("/api/pair", None, br#"{"code":"999999"}"#).await;
        }
        let (status, _) = host.api("/api/pair", None, br#"{"code":"314159"}"#).await;
        assert_eq!(status, 429);
    }

    #[tokio::test]
    async fn serves_the_page_over_tls() {
        let dir = std::env::temp_dir().join(format!("mrt-companion-{}", uuid::Uuid::new_v4()));
        let (cert, key, fingerprint) = load_or_create_certificate(&dir).unwrap();
        assert_eq!(load_or_create_certificate(&dir).unwrap().2, fingerprint, "the certificate is kept");
        assert_eq!(fingerprint.split(':').count(), 32);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let host = companion(true).await;
        tokio::spawn(async move { serve_companion(listener, &cert, &key, host).await });
        // A TLS client that accepts the self-signed certificate (as the user does after
        // checking the fingerprint).
        #[derive(Debug)]
        struct AcceptAny;
        impl tokio_rustls::rustls::client::danger::ServerCertVerifier for AcceptAny {
            fn verify_server_cert(&self, _: &tokio_rustls::rustls::pki_types::CertificateDer<'_>, _: &[tokio_rustls::rustls::pki_types::CertificateDer<'_>], _: &tokio_rustls::rustls::pki_types::ServerName<'_>, _: &[u8], _: tokio_rustls::rustls::pki_types::UnixTime) -> Result<tokio_rustls::rustls::client::danger::ServerCertVerified, tokio_rustls::rustls::Error> {
                Ok(tokio_rustls::rustls::client::danger::ServerCertVerified::assertion())
            }
            fn verify_tls12_signature(&self, _: &[u8], _: &tokio_rustls::rustls::pki_types::CertificateDer<'_>, _: &tokio_rustls::rustls::DigitallySignedStruct) -> Result<tokio_rustls::rustls::client::danger::HandshakeSignatureValid, tokio_rustls::rustls::Error> {
                Ok(tokio_rustls::rustls::client::danger::HandshakeSignatureValid::assertion())
            }
            fn verify_tls13_signature(&self, _: &[u8], _: &tokio_rustls::rustls::pki_types::CertificateDer<'_>, _: &tokio_rustls::rustls::DigitallySignedStruct) -> Result<tokio_rustls::rustls::client::danger::HandshakeSignatureValid, tokio_rustls::rustls::Error> {
                Ok(tokio_rustls::rustls::client::danger::HandshakeSignatureValid::assertion())
            }
            fn supported_verify_schemes(&self) -> Vec<tokio_rustls::rustls::SignatureScheme> {
                tokio_rustls::rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
            }
        }
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let config = tokio_rustls::rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny))
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut tls = connector.connect(tokio_rustls::rustls::pki_types::ServerName::try_from("localhost").unwrap(), tcp).await.unwrap();
        tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut page = Vec::new();
        let _ = tls.read_to_end(&mut page).await;
        let page = String::from_utf8_lossy(&page);
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("MRT companion"), "{}", &page[..page.len().min(200)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
