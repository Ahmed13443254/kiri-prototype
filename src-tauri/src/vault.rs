// The model API key. It is the one credential this product holds, and the
// architecture is explicit about where it may live: the OS keychain, never the
// database (.sqlite), never a settings file, never a log line, never the
// repository. The renderer never sees it back -- `key_present` is the only
// thing it may read.
//
// The keychain is the same OS store the user already trusts for their other
// apps, reached through the `keyring` crate: Windows Credential Manager,
// macOS Keychain, or Secret Service on Linux guidance.

use keyring::Entry;
use std::sync::atomic::{AtomicBool, Ordering};

const SERVICE: &str = "app.kiri.desktop";
const ACCOUNT: &str = "model-api-key";

/// Read the stored key. `None` covers "no keychain entry" and "keychain
/// unavailable" alike: neither is a credential the caller may use.
pub fn read_key() -> Option<String> {
    entry().ok().and_then(|e| e.get_password().ok())
}

pub struct KeyVault {
    present: AtomicBool,
}

impl KeyVault {
    /// `present` starts false and is refreshed by `probe`, so a UI launched
    /// without a key never pretends to have one.
    pub fn new() -> Self {
        Self {
            present: AtomicBool::new(false),
        }
    }

    /// Ask the OS store, once at boot, whether a key exists. Never returns it.
    pub fn probe(&self) -> bool {
        let present = entry().and_then(|e| e.get_password()).is_ok();
        self.present.store(present, Ordering::SeqCst);
        present
    }

    pub fn is_present(&self) -> bool {
        self.present.load(Ordering::SeqCst)
    }

    /// Store a new key. Empty input is a clear, not an empty secret: some
    /// keychains accept "" and then every request fails 401 for a reason the
    /// user cannot see.
    pub fn set(&self, key: &str) -> Result<(), String> {
        let key = key.trim();
        if key.is_empty() {
            return self.clear();
        }
        let e = entry().map_err(|e| e.to_string())?;
        e.set_password(key).map_err(|e| e.to_string())?;
        self.present.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// The engine's stdin `initialize` carries the key. This is the only read.
    pub fn get(&self) -> Option<String> {
        entry().ok().and_then(|e| e.get_password().ok())
    }

    pub fn clear(&self) -> Result<(), String> {
        let e = entry().map_err(|e| e.to_string())?;
        // Deleting a key that is not there is success for the caller.
        match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                self.present.store(false, Ordering::SeqCst);
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Exposed for the settings screen: the store's own name, not a path.
    pub fn backend(&self) -> &'static str {
        if cfg!(windows) {
            "Windows Credential Manager"
        } else if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else {
            "Secret Service"
        }
    }
}

fn entry() -> keyring::Result<Entry> {
    Entry::new(SERVICE, ACCOUNT)
}
