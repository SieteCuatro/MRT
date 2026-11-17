//! Remote connections between MRT instances (`mrt share`, `mrt pair`, `mrt attach`).
//!
//! The host runs `mrt share`, an encrypted relay in front of its local bridge: a
//! paired guest's JSON-RPC requests (the same MCP tools the CLI and agents use) are
//! forwarded to the running MRT window or server and the answers sent back.
//!
//! Security:
//! - **Pairing.** The host shows a 6-digit code. The guest types it; both run SPAKE2
//!   on it, so a wrong code (or a listener on the network) learns nothing useful, and
//!   the derived key is the pre-shared key of a `Noise_XXpsk3` handshake that also
//!   exchanges both sides' long-term X25519 keys. The host then asks its user to
//!   approve the device by name, and both sides store the other's key.
//! - **Later connections** use `Noise_XX` and are accepted only when the peer's
//!   static key is in the paired list. Network identity (IP, name) is never trusted.
//! - **Revocation** removes a device's key; its live connections end at the next
//!   request.
//!
//! Wire format: a mode byte (`1` pairing, `2` paired), then frames of a 2-byte
//! big-endian length and a Noise message.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use snow::{Builder, HandshakeState, TransportState};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const PAIRING_PATTERN: &str = "Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s";
const PAIRED_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const MODE_PAIRING: u8 = 1;
const MODE_PAIRED: u8 = 2;
const MAX_FRAME: usize = 65_535;
/// Largest JSON-RPC message relayed (split over several Noise frames).
const MAX_MESSAGE: usize = 64 * 1024 * 1024;
/// The default `mrt share` port.
pub const DEFAULT_SHARE_PORT: u16 = 7682;

/// A paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub name: String,
    /// The device's X25519 public key, hex.
    pub key: String,
    /// `host:port` for devices this side connects to (guest side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// This side's key pair and paired devices (`remote.json` in the data folder).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairStore {
    pub private_key: String,
    pub public_key: String,
    /// Devices allowed to attach to this host.
    #[serde(default)]
    pub guests: Vec<PairedDevice>,
    /// Hosts this device has paired with.
    #[serde(default)]
    pub hosts: Vec<PairedDevice>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// Default location of the pair store.
pub fn pair_store_path() -> Option<PathBuf> {
    mrt_core::data_dir().map(|dir| dir.join("remote.json"))
}

impl PairStore {
    /// Loads the store at `path`, creating a key pair the first time.
    pub fn load(path: &Path) -> Result<Self> {
        let mut store: PairStore = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).context("remote.json is not valid")?,
            Err(_) => {
                let keys = Builder::new(PAIRED_PATTERN.parse()?).generate_keypair()?;
                PairStore { private_key: hex(&keys.private), public_key: hex(&keys.public), ..Default::default() }
            }
        };
        store.path = Some(path.to_path_buf());
        store.save()?;
        Ok(store)
    }

    /// An in-memory store with a fresh key pair (tests).
    pub fn ephemeral() -> Result<Self> {
        let keys = Builder::new(PAIRED_PATTERN.parse()?).generate_keypair()?;
        Ok(PairStore { private_key: hex(&keys.private), public_key: hex(&keys.public), ..Default::default() })
    }

    /// Saves only the paired guests, keeping what another MRT process (`mrt pair`)
    /// wrote to the file meanwhile.
    pub fn save_guests(&self) -> Result<()> {
        self.save_part(true)
    }

    /// Saves only the paired hosts (see [`save_guests`](Self::save_guests)).
    pub fn save_hosts(&self) -> Result<()> {
        self.save_part(false)
    }

    fn save_part(&self, guests: bool) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        let on_disk = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<PairStore>(&t).ok());
        match on_disk {
            Some(mut merged) => {
                if guests {
                    merged.guests = self.guests.clone();
                } else {
                    merged.hosts = self.hosts.clone();
                }
                merged.path = Some(path.clone());
                merged.save()
            }
            None => self.save(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    fn private(&self) -> Result<Vec<u8>> {
        unhex(&self.private_key).ok_or_else(|| anyhow!("remote.json has an invalid key"))
    }

    pub fn is_guest(&self, key: &[u8]) -> Option<&PairedDevice> {
        let key = hex(key);
        self.guests.iter().find(|g| g.key == key)
    }

    /// Removes a paired guest by name; returns whether one was removed.
    pub fn revoke(&mut self, name: &str) -> bool {
        let before = self.guests.len();
        self.guests.retain(|g| !g.name.eq_ignore_ascii_case(name));
        before != self.guests.len()
    }
}

/// A random 6-digit pairing code.
pub fn new_pairing_code() -> String {
    let mut bytes = [0u8; 4];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
    format!("{:06}", u32::from_le_bytes(bytes) % 1_000_000)
}

async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    if data.len() > MAX_FRAME {
        bail!("frame too large");
    }
    stream.write_all(&(data.len() as u16).to_be_bytes()).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len).await?;
    let mut data = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut data).await?;
    Ok(data)
}

fn spake2_start(code: &str) -> (spake2::Spake2<spake2::Ed25519Group>, Vec<u8>) {
    spake2::Spake2::<spake2::Ed25519Group>::start_symmetric(&spake2::Password::new(code.as_bytes()), &spake2::Identity::new(b"mrt-share"))
}

async fn handshake_send(stream: &mut TcpStream, hs: &mut HandshakeState, payload: &[u8]) -> Result<()> {
    let mut buf = vec![0u8; MAX_FRAME];
    let n = hs.write_message(payload, &mut buf)?;
    write_frame(stream, &buf[..n]).await
}

async fn handshake_recv(stream: &mut TcpStream, hs: &mut HandshakeState) -> Result<Vec<u8>> {
    let frame = read_frame(stream).await?;
    let mut buf = vec![0u8; MAX_FRAME];
    let n = hs.read_message(&frame, &mut buf).map_err(|_| anyhow!("handshake failed (wrong pairing code or unknown key)"))?;
    buf.truncate(n);
    Ok(buf)
}

/// An encrypted, authenticated connection.
pub struct SecureChannel {
    stream: TcpStream,
    transport: TransportState,
    /// The peer's static public key.
    pub peer_key: Vec<u8>,
}

impl SecureChannel {
    /// Sends one message (split into frames; an empty frame ends it).
    pub async fn send(&mut self, message: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; MAX_FRAME];
        for chunk in message.chunks(MAX_FRAME - 64) {
            let n = self.transport.write_message(chunk, &mut buf)?;
            write_frame(&mut self.stream, &buf[..n]).await?;
        }
        let n = self.transport.write_message(&[], &mut buf)?;
        write_frame(&mut self.stream, &buf[..n]).await
    }

    pub async fn recv(&mut self) -> Result<Vec<u8>> {
        let mut message = Vec::new();
        let mut buf = vec![0u8; MAX_FRAME];
        loop {
            let frame = read_frame(&mut self.stream).await?;
            let n = self.transport.read_message(&frame, &mut buf).map_err(|_| anyhow!("message failed to decrypt"))?;
            if n == 0 {
                return Ok(message);
            }
            if message.len() + n > MAX_MESSAGE {
                bail!("message too large");
            }
            message.extend_from_slice(&buf[..n]);
        }
    }

    /// Sends a JSON-RPC request and returns the response (guest side).
    pub async fn call(&mut self, request: &crate::protocol::JsonRpcRequest) -> Result<crate::protocol::JsonRpcResponse> {
        self.send(&serde_json::to_vec(request)?).await?;
        let reply = self.recv().await?;
        Ok(serde_json::from_slice(&reply)?)
    }
}

/// Guest: pairs with a host using the code it shows. Returns the host's name and key;
/// the caller stores them.
pub async fn pair_with_host(address: &str, code: &str, device_name: &str, store: &PairStore) -> Result<(String, Vec<u8>)> {
    let mut stream = TcpStream::connect(address).await.with_context(|| format!("cannot reach {address}"))?;
    stream.write_all(&[MODE_PAIRING]).await?;
    let (spake, outbound) = spake2_start(code);
    write_frame(&mut stream, &outbound).await?;
    let inbound = read_frame(&mut stream).await?;
    let psk = spake.finish(&inbound).map_err(|_| anyhow!("pairing failed"))?;
    let mut hs = Builder::new(PAIRING_PATTERN.parse()?).local_private_key(&store.private()?).psk(3, &psk[..32]).build_initiator()?;
    handshake_send(&mut stream, &mut hs, &[]).await?;
    let _ = handshake_recv(&mut stream, &mut hs).await?;
    handshake_send(&mut stream, &mut hs, device_name.as_bytes()).await?;
    let host_key = hs.get_remote_static().ok_or_else(|| anyhow!("host sent no key"))?.to_vec();
    let mut channel = SecureChannel { stream, transport: hs.into_transport_mode()?, peer_key: host_key.clone() };
    let verdict = channel.recv().await?;
    let verdict: serde_json::Value = serde_json::from_slice(&verdict)?;
    if verdict["approved"].as_bool() != Some(true) {
        bail!("the host did not approve this device");
    }
    Ok((verdict["host"].as_str().unwrap_or("host").to_string(), host_key))
}

/// Guest: connects to a paired host.
pub async fn connect_to_host(address: &str, host_key: &[u8], store: &PairStore) -> Result<SecureChannel> {
    let mut stream = TcpStream::connect(address).await.with_context(|| format!("cannot reach {address}"))?;
    stream.write_all(&[MODE_PAIRED]).await?;
    let mut hs = Builder::new(PAIRED_PATTERN.parse()?).local_private_key(&store.private()?).build_initiator()?;
    handshake_send(&mut stream, &mut hs, &[]).await?;
    let _ = handshake_recv(&mut stream, &mut hs).await?;
    let remote = hs.get_remote_static().ok_or_else(|| anyhow!("host sent no key"))?.to_vec();
    if remote != host_key {
        bail!("the host's key changed; pair again if you trust it");
    }
    handshake_send(&mut stream, &mut hs, &[]).await?;
    let mut channel = SecureChannel { stream, transport: hs.into_transport_mode()?, peer_key: remote };
    // The host tells us whether it still accepts our key.
    let hello: serde_json::Value = serde_json::from_slice(&channel.recv().await?)?;
    if hello["accepted"].as_bool() != Some(true) {
        bail!("the host does not accept this device (not paired or revoked)");
    }
    Ok(channel)
}

/// Host settings for [`serve_share`].
pub struct ShareHost {
    pub store: Arc<Mutex<PairStore>>,
    /// Current pairing code (`None`: pairing closed).
    pub pairing_code: Option<String>,
    pub host_name: String,
    /// Asked for each new device: `(device name) -> approve?`.
    pub approve: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Where requests go: the local bridge (port, token).
    pub bridge: (u16, String),
}

/// Host: accepts guests on `listener` until the task is dropped.
pub async fn serve_share(listener: TcpListener, host: Arc<ShareHost>) {
    while let Ok((stream, peer)) = listener.accept().await {
        let host = host.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_guest(stream, &host).await {
                tracing::info!("remote connection from {peer} ended: {error}");
            }
        });
    }
}

async fn handle_guest(mut stream: TcpStream, host: &ShareHost) -> Result<()> {
    let private = host.store.lock().map_err(|_| anyhow!("store lock"))?.private()?;
    let mut mode = [0u8; 1];
    stream.read_exact(&mut mode).await?;
    let mut channel = match mode[0] {
        MODE_PAIRING => {
            let Some(code) = &host.pairing_code else { bail!("pairing is closed") };
            let (spake, outbound) = spake2_start(code);
            let inbound = read_frame(&mut stream).await?;
            write_frame(&mut stream, &outbound).await?;
            let psk = spake.finish(&inbound).map_err(|_| anyhow!("pairing failed"))?;
            let mut hs = Builder::new(PAIRING_PATTERN.parse()?).local_private_key(&private).psk(3, &psk[..32]).build_responder()?;
            let _ = handshake_recv(&mut stream, &mut hs).await?;
            handshake_send(&mut stream, &mut hs, &[]).await?;
            let name = String::from_utf8_lossy(&handshake_recv(&mut stream, &mut hs).await?).trim().to_string();
            let key = hs.get_remote_static().ok_or_else(|| anyhow!("guest sent no key"))?.to_vec();
            let mut channel = SecureChannel { stream, transport: hs.into_transport_mode()?, peer_key: key.clone() };
            let name = if name.is_empty() { "device".to_string() } else { name };
            let approve = host.approve.clone();
            let asked = name.clone();
            let approved = tokio::task::spawn_blocking(move || approve(&asked)).await.unwrap_or(false);
            if approved {
                let mut store = host.store.lock().map_err(|_| anyhow!("store lock"))?;
                store.guests.retain(|g| g.key != hex(&key));
                store.guests.push(PairedDevice { name: name.clone(), key: hex(&key), address: None });
                store.save_guests()?;
            }
            channel.send(&serde_json::to_vec(&serde_json::json!({ "approved": approved, "host": host.host_name }))?).await?;
            return Ok(());
        }
        MODE_PAIRED => {
            let mut hs = Builder::new(PAIRED_PATTERN.parse()?).local_private_key(&private).build_responder()?;
            let _ = handshake_recv(&mut stream, &mut hs).await?;
            handshake_send(&mut stream, &mut hs, &[]).await?;
            let _ = handshake_recv(&mut stream, &mut hs).await?;
            let key = hs.get_remote_static().ok_or_else(|| anyhow!("guest sent no key"))?.to_vec();
            let mut channel = SecureChannel { stream, transport: hs.into_transport_mode()?, peer_key: key.clone() };
            let accepted = host.store.lock().map_err(|_| anyhow!("store lock"))?.is_guest(&key).is_some();
            channel.send(&serde_json::to_vec(&serde_json::json!({ "accepted": accepted, "host": host.host_name }))?).await?;
            if !accepted {
                bail!("unknown device key");
            }
            channel
        }
        other => bail!("unknown mode {other}"),
    };
    loop {
        let message = channel.recv().await?;
        // Revoked while connected: stop serving.
        if host.store.lock().map_err(|_| anyhow!("store lock"))?.is_guest(&channel.peer_key).is_none() {
            bail!("device revoked");
        }
        let request: crate::protocol::JsonRpcRequest = serde_json::from_slice(&message)?;
        let response = match crate::bridge::try_forward_jsonrpc_to_gui(host.bridge.0, &host.bridge.1, &request).await {
            Ok(Some(response)) => response,
            Ok(None) => crate::protocol::JsonRpcResponse::success(request.id.clone(), serde_json::json!({})),
            Err(error) => crate::protocol::JsonRpcResponse::error(request.id.clone(), -32000, format!("host MRT is not reachable: {error}")),
        };
        channel.send(&serde_json::to_vec(&response)?).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(name: &str) -> crate::protocol::JsonRpcRequest {
        crate::protocol::JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(7)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": {} })),
        }
    }

    async fn host(approve: bool) -> (String, Arc<ShareHost>) {
        let state = crate::server::MrtMcpServerState::default();
        let bridge = crate::bridge::start_gui_bridge_listener(state, 0, false).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let host = Arc::new(ShareHost {
            store: Arc::new(Mutex::new(PairStore::ephemeral().unwrap())),
            pairing_code: Some("123456".into()),
            host_name: "workstation".into(),
            approve: Arc::new(move |_| approve),
            bridge: (bridge.addr.port(), bridge.token),
        });
        tokio::spawn(serve_share(listener, host.clone()));
        (address, host)
    }

    #[tokio::test]
    async fn pairing_then_encrypted_requests_and_revocation() {
        let (address, host) = host(true).await;
        let guest = PairStore::ephemeral().unwrap();
        assert!(pair_with_host(&address, "000000", "laptop", &guest).await.is_err(), "wrong code");
        let (host_name, host_key) = pair_with_host(&address, "123456", "laptop", &guest).await.unwrap();
        assert_eq!(host_name, "workstation");
        assert_eq!(host.store.lock().unwrap().guests[0].name, "laptop");

        let mut channel = connect_to_host(&address, &host_key, &guest).await.unwrap();
        let response = channel.call(&request("mrt_list_tabs")).await.unwrap();
        let text = response.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("\"index\""), "{text}");

        let stranger = PairStore::ephemeral().unwrap();
        assert!(connect_to_host(&address, &host_key, &stranger).await.is_err(), "unpaired keys are refused");

        host.store.lock().unwrap().revoke("laptop");
        assert!(channel.call(&request("mrt_list_tabs")).await.is_err(), "a revoked device's live connection ends");
        assert!(connect_to_host(&address, &host_key, &guest).await.is_err());
    }

    #[tokio::test]
    async fn the_host_user_must_approve_a_new_device() {
        let (address, host) = host(false).await;
        let guest = PairStore::ephemeral().unwrap();
        assert!(pair_with_host(&address, "123456", "phone", &guest).await.is_err());
        assert!(host.store.lock().unwrap().guests.is_empty());
    }

    #[test]
    fn saving_one_side_keeps_the_other() {
        let path = std::env::temp_dir().join(format!("mrt-remote-{}", uuid::Uuid::new_v4())).join("remote.json");
        let mut host_view = PairStore::load(&path).unwrap();
        let mut guest_view = PairStore::load(&path).unwrap();
        guest_view.hosts.push(PairedDevice { name: "box".into(), key: "aa".into(), address: Some("box:7682".into()) });
        guest_view.save_hosts().unwrap();
        host_view.guests.push(PairedDevice { name: "laptop".into(), key: "bb".into(), address: None });
        host_view.save_guests().unwrap();
        let both = PairStore::load(&path).unwrap();
        assert_eq!(both.hosts.len(), 1);
        assert_eq!(both.guests.len(), 1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn codes_and_store_files() {
        let code = new_pairing_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        let path = std::env::temp_dir().join(format!("mrt-remote-{}", uuid::Uuid::new_v4())).join("remote.json");
        let store = PairStore::load(&path).unwrap();
        let again = PairStore::load(&path).unwrap();
        assert_eq!(store.public_key, again.public_key, "the key pair persists");
        assert_eq!(unhex(&hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
