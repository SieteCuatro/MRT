//! Provider API keys: from the environment first, then from the OS keyring (Windows
//! Credential Manager, the macOS Keychain, or the Secret Service on Linux), so MRT
//! started from a desktop launcher, without the shell's environment, still finds
//! them. Keys are stored under the service `mrt` with the variable name as the user
//! (`mrt` / `OPENAI_API_KEY`).
//!
//! The keyring is only read for the names in [`API_KEYS`], once per name (the result
//! is cached until a key is stored or removed), and always on a plain thread of its
//! own: the Secret Service backend runs its own async runtime and must not be called
//! from inside a Tokio task.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// The keyring service name MRT's entries use.
pub const KEYRING_SERVICE: &str = "mrt";

/// Set to any non-empty value to never touch the OS keyring.
pub const NO_KEYRING_ENV: &str = "MRT_NO_KEYRING";

/// The API key variables MRT reads, with the provider they belong to.
pub const API_KEYS: &[(&str, &str)] = &[
    ("GEMINI_API_KEY", "Google Gemini"),
    ("ANTHROPIC_API_KEY", "Anthropic Claude"),
    ("OPENAI_API_KEY", "OpenAI"),
    ("XAI_API_KEY", "xAI Grok"),
    ("DEEPSEEK_API_KEY", "DeepSeek"),
    ("OPENROUTER_API_KEY", "OpenRouter"),
];

/// Where a key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The environment variable is set (it wins over the keyring).
    Environment,
    Keyring,
    Missing,
}

impl KeySource {
    pub fn label(self) -> &'static str {
        match self {
            KeySource::Environment => "from the environment",
            KeySource::Keyring => keyring_name(),
            KeySource::Missing => "not set",
        }
    }
}

/// The name of the platform's keyring, for messages.
pub fn keyring_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "in Windows Credential Manager"
    } else if cfg!(target_os = "macos") {
        "in the macOS Keychain"
    } else {
        "in the Secret Service keyring"
    }
}

/// Where secrets are kept: the OS keyring, or another store in tests.
pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, secret: &str) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// The OS keyring through the `keyring` crate.
struct OsKeyring;

/// Runs `f` on a new thread (no async runtime in it) and waits for it.
fn on_plain_thread<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    std::thread::spawn(f).join().map_err(|_| "the keyring call panicked".to_string())?
}

impl SecretStore for OsKeyring {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        let name = name.to_string();
        on_plain_thread(move || {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &name).map_err(|e| e.to_string())?;
            match entry.get_password() {
                Ok(secret) => Ok(Some(secret)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        })
    }

    fn set(&self, name: &str, secret: &str) -> Result<(), String> {
        let (name, secret) = (name.to_string(), secret.to_string());
        on_plain_thread(move || {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &name).map_err(|e| e.to_string())?;
            entry.set_password(&secret).map_err(|e| e.to_string())
        })
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let name = name.to_string();
        on_plain_thread(move || {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &name).map_err(|e| e.to_string())?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        })
    }
}

/// A store that keeps nothing (`MRT_NO_KEYRING`).
struct NoKeyring;

impl SecretStore for NoKeyring {
    fn get(&self, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn set(&self, _: &str, _: &str) -> Result<(), String> {
        Err(format!("the keyring is turned off ({NO_KEYRING_ENV} is set)"))
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

/// An in-memory store, for tests.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().map_err(|e| e.to_string())?.get(name).cloned())
    }
    fn set(&self, name: &str, secret: &str) -> Result<(), String> {
        self.0.lock().map_err(|e| e.to_string())?.insert(name.into(), secret.into());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), String> {
        self.0.lock().map_err(|e| e.to_string())?.remove(name);
        Ok(())
    }
}

static STORE: RwLock<Option<Arc<dyn SecretStore>>> = RwLock::new(None);
static CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

fn store() -> Arc<dyn SecretStore> {
    if let Some(store) = STORE.read().ok().and_then(|s| s.clone()) {
        return store;
    }
    let store: Arc<dyn SecretStore> =
        if std::env::var_os(NO_KEYRING_ENV).is_some_and(|v| !v.is_empty()) { Arc::new(NoKeyring) } else { Arc::new(OsKeyring) };
    if let Ok(mut slot) = STORE.write() {
        slot.get_or_insert_with(|| store.clone());
    }
    store
}

/// Replaces the secret store (tests) and forgets cached keys.
pub fn set_secret_store(new: Arc<dyn SecretStore>) {
    if let Ok(mut slot) = STORE.write() {
        *slot = Some(new);
    }
    if let Ok(mut cache) = CACHE.lock() {
        *cache = None;
    }
}

fn is_api_key(name: &str) -> bool {
    API_KEYS.iter().any(|(key, _)| *key == name)
}

fn from_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn cache_put(name: &str, value: Option<String>) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.get_or_insert_with(HashMap::new).insert(name.to_string(), value);
    }
}

/// The keyring's value for `name` (cached; errors count as no key).
fn from_keyring(name: &str) -> Option<String> {
    if let Some(cached) = CACHE.lock().ok().and_then(|c| c.as_ref().and_then(|c| c.get(name).cloned())) {
        return cached;
    }
    let value = match store().get(name) {
        Ok(value) => value.filter(|v| !v.trim().is_empty()),
        Err(error) => {
            tracing::debug!("keyring lookup of {name} failed: {error}");
            None
        }
    };
    cache_put(name, value.clone());
    value
}

/// The value of variable `name`: the environment, else (for API keys) the keyring.
/// The lookup `plan_ai_backends` uses.
pub fn lookup(name: &str) -> Option<String> {
    from_env(name).or_else(|| if is_api_key(name) { from_keyring(name) } else { None })
}

/// Where API key `name` comes from.
pub fn key_source(name: &str) -> KeySource {
    if from_env(name).is_some() {
        KeySource::Environment
    } else if from_keyring(name).is_some() {
        KeySource::Keyring
    } else {
        KeySource::Missing
    }
}

/// Stores API key `name` in the keyring.
pub fn store_api_key(name: &str, secret: &str) -> Result<(), String> {
    if !is_api_key(name) {
        return Err(format!("{name} is not an API key MRT uses"));
    }
    let secret = secret.trim();
    if secret.is_empty() {
        return Err("the key is empty".into());
    }
    store().set(name, secret)?;
    cache_put(name, Some(secret.to_string()));
    Ok(())
}

/// Removes API key `name` from the keyring (the environment is not touched).
pub fn delete_api_key(name: &str) -> Result<(), String> {
    if !is_api_key(name) {
        return Err(format!("{name} is not an API key MRT uses"));
    }
    store().delete(name)?;
    cache_put(name, None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes, reads and deletes `mrt` / `MRT_KEYRING_SELFTEST` in the real OS
    /// keyring: `cargo test -p mrt-ai -- --ignored os_keyring`.
    #[test]
    #[ignore = "uses the real OS keyring"]
    fn os_keyring_round_trip() {
        let name = "MRT_KEYRING_SELFTEST";
        let os = OsKeyring;
        os.set(name, "selftest-value").unwrap();
        assert_eq!(os.get(name).unwrap().as_deref(), Some("selftest-value"));
        os.delete(name).unwrap();
        assert_eq!(os.get(name).unwrap(), None);
        os.delete(name).unwrap();
    }

    #[test]
    fn keys_come_from_the_environment_then_the_keyring() {
        set_secret_store(Arc::new(MemoryStore::default()));
        // A variable no environment sets, so the keyring decides.
        let name = "OPENROUTER_API_KEY";
        if from_env(name).is_some() {
            return;
        }
        assert_eq!(lookup(name), None);
        assert_eq!(key_source(name), KeySource::Missing);
        store_api_key(name, "  or-secret \n").unwrap();
        assert_eq!(lookup(name).as_deref(), Some("or-secret"));
        assert_eq!(key_source(name), KeySource::Keyring);
        delete_api_key(name).unwrap();
        assert_eq!(lookup(name), None);

        assert!(store_api_key("PATH", "x").is_err(), "only API keys go in the keyring");
        assert!(store_api_key(name, "  ").is_err());
        assert_eq!(lookup("MRT_SURELY_UNSET_VARIABLE"), None);
    }
}
