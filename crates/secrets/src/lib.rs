//! Secrets kept in the Secret Service (GNOME Keyring, KWallet): the
//! repository master key, so automatic backups need no password, and the
//! Google refresh token (PLAN.md 4.5). The password itself is never stored.
//!
//! Without a Secret Service, nothing is stored: the app asks for the
//! password at each backup and automatic backup is unavailable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use keyring_core::{Entry, Error as KeyringError};

const SERVICE: &str = "xkyhold-lx";

#[derive(Debug)]
pub struct SecretsError(pub String);

impl std::fmt::Display for SecretsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the keyring could not be used: {}", self.0)
    }
}

impl std::error::Error for SecretsError {}

pub type Result<T> = std::result::Result<T, SecretsError>;

/// Where secrets live. Keys are short ASCII names such as
/// `repo-key:<repo id>` or `google-refresh:<account>`.
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, key: &str, secret: &[u8]) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

pub fn repo_key(repo_id: &str) -> String {
    format!("repo-key:{repo_id}")
}

pub fn google_refresh(account: &str) -> String {
    format!("google-refresh:{account}")
}

/// The desktop's Secret Service over D-Bus.
pub struct SecretService;

impl SecretService {
    /// Connects to the Secret Service; fails if there is none. A session
    /// bus alone is not enough: a lookup must answer (a fresh user started
    /// with `machinectl shell` has the bus and no keyring daemon).
    pub fn connect() -> Result<Self> {
        let store = zbus_secret_service_keyring_store::Store::new().map_err(|e| SecretsError(e.to_string()))?;
        keyring_core::set_default_store(store);
        let service = SecretService;
        service.get("probe")?;
        Ok(service)
    }

    fn entry(key: &str) -> Result<Entry> {
        Entry::new(SERVICE, key).map_err(|e| SecretsError(e.to_string()))
    }
}

impl SecretStore for SecretService {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match Self::entry(key)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(e) => Err(SecretsError(e.to_string())),
        }
    }

    fn set(&self, key: &str, secret: &[u8]) -> Result<()> {
        Self::entry(key)?.set_secret(secret).map_err(|e| SecretsError(e.to_string()))
    }

    fn delete(&self, key: &str) -> Result<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(e) => Err(SecretsError(e.to_string())),
        }
    }
}

/// Secrets in memory, for tests and for a session without a Secret Service.
#[derive(Default, Clone)]
pub struct MemorySecrets(Arc<Mutex<HashMap<String, Vec<u8>>>>);

impl SecretStore for MemorySecrets {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }

    fn set(&self, key: &str, secret: &[u8]) -> Result<()> {
        self.0.lock().unwrap().insert(key.to_owned(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
