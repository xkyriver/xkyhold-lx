//! Cryptography for the repository format (FORMAT.md, section 4).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::bytes::{from_base32_groups, from_base64, to_base32_groups, to_base64};
use crate::{Error, Result};

pub const NONCE_LENGTH: usize = 12;
pub const TAG_LENGTH: usize = 16;
pub const KEY_LENGTH: usize = 32;
/// Bytes added to a plaintext by [`seal`].
pub const SEAL_OVERHEAD: usize = NONCE_LENGTH + TAG_LENGTH;

/// Formats this engine can read.
pub const READABLE_FORMATS: &[u32] = &[1, 2];
/// The only format this engine writes into (FORMAT-v2.md, section 7).
pub const WRITE_FORMAT: u32 = 2;
pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

/// The working keys of an unlocked repository. Wiped on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RepoKeys {
    #[zeroize(skip)]
    pub repo_id: String,
    enc_key: [u8; KEY_LENGTH],
    id_key: [u8; KEY_LENGTH],
}

impl RepoKeys {
    /// Derives the working keys from the master key (section 4.1).
    pub fn derive(master_key: &[u8], repo_id: &str) -> Self {
        Self {
            repo_id: repo_id.to_owned(),
            enc_key: hkdf_sha256(master_key, &format!("xkyhold/v1/enc/{repo_id}")),
            id_key: hkdf_sha256(master_key, &format!("xkyhold/v1/id/{repo_id}")),
        }
    }

    /// HMAC-SHA-256(idKey, plaintext) as hex: the blob's identity.
    pub fn blob_id(&self, plaintext: &[u8]) -> String {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.id_key).expect("any key length");
        mac.update(plaintext);
        hex::encode(mac.finalize().into_bytes())
    }

    /// Encrypts with a fresh random nonce.
    pub fn seal(&self, plaintext: &[u8], aad: &str) -> Vec<u8> {
        seal(&self.enc_key, plaintext, aad, &random_array())
    }

    pub fn open(&self, sealed: &[u8], aad: &str) -> Result<Vec<u8>> {
        open(&self.enc_key, sealed, aad)
    }
}

/// Bytes from the operating system's random generator.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    getrandom::fill(&mut out).expect("the system random generator is available");
    out
}

pub fn random_array<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the system random generator is available");
    out
}

/// 16 random bytes as hex: repository, snapshot and lock ids.
pub fn random_id() -> String {
    hex::encode(random_array::<16>())
}

fn hkdf_sha256(ikm: &[u8], info: &str) -> [u8; KEY_LENGTH] {
    let mut out = [0u8; KEY_LENGTH];
    Hkdf::<Sha256>::new(Some(&[]), ikm)
        .expand(info.as_bytes(), &mut out)
        .expect("32 bytes is a valid HKDF output length");
    out
}

/// `nonce || ciphertext || tag`, bound to `aad` (the object's role).
pub fn seal(key: &[u8; KEY_LENGTH], plaintext: &[u8], aad: &str, nonce: &[u8; NONCE_LENGTH]) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let ciphertext = cipher
        .encrypt(&Nonce::from(*nonce), Payload { msg: plaintext, aad: aad.as_bytes() })
        .expect("AES-GCM encryption does not fail for in-range lengths");
    let mut out = Vec::with_capacity(NONCE_LENGTH + ciphertext.len());
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ciphertext);
    out
}

pub fn open(key: &[u8; KEY_LENGTH], sealed: &[u8], aad: &str) -> Result<Vec<u8>> {
    if sealed.len() < SEAL_OVERHEAD {
        return Err(Error::Authentication("encrypted object is too short"));
    }
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let (nonce, body) = sealed.split_at(NONCE_LENGTH);
    cipher
        .decrypt(&Nonce::try_from(nonce).expect("12-byte nonce"), Payload { msg: body, aad: aad.as_bytes() })
        .map_err(|_| Error::Authentication("data failed authentication"))
}

// --- Config and key slots ------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Argon2Params {
    #[serde(rename = "memoryKiB")]
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

pub const DEFAULT_ARGON2: Argon2Params = Argon2Params { memory_kib: 64 * 1024, iterations: 3, parallelism: 1 };

/// One way of unwrapping the master key. Unknown slot types are kept as
/// raw JSON so that rewriting `config` never drops them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeySlot {
    Known(KnownSlot),
    Unknown(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KnownSlot {
    #[serde(rename = "password", rename_all = "camelCase")]
    Password {
        kdf: String,
        #[serde(flatten)]
        params: Argon2Params,
        salt: String,
        wrapped_master_key: String,
    },
    #[serde(rename = "recovery", rename_all = "camelCase")]
    Recovery { kdf: String, wrapped_master_key: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunker {
    #[serde(rename = "type")]
    pub kind: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoConfig {
    pub format: u32,
    pub repo_id: String,
    pub created: String,
    pub chunker: Chunker,
    pub keys: Vec<KeySlot>,
}

/// What opens a repository: its password or its recovery key.
pub enum Credential {
    Password(Zeroizing<String>),
    RecoveryKey(Zeroizing<String>),
}

fn key_aad(repo_id: &str) -> String {
    format!("xkyhold/v1/key/{repo_id}")
}

fn password_kek(password: &str, salt: &[u8], params: &Argon2Params) -> Result<Zeroizing<[u8; KEY_LENGTH]>> {
    use unicode_normalization::UnicodeNormalization;
    let argon = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(params.memory_kib, params.iterations, params.parallelism, Some(KEY_LENGTH))
            .map_err(|e| Error::Format(format!("invalid Argon2 parameters: {e}")))?,
    );
    let mut out = Zeroizing::new([0u8; KEY_LENGTH]);
    let normalized = Zeroizing::new(password.nfc().collect::<String>());
    argon
        .hash_password_into(normalized.as_bytes(), salt, out.as_mut())
        .map_err(|e| Error::Format(format!("Argon2 failed: {e}")))?;
    Ok(out)
}

fn recovery_kek(recovery_key: &[u8], repo_id: &str) -> Zeroizing<[u8; KEY_LENGTH]> {
    Zeroizing::new(hkdf_sha256(recovery_key, &format!("xkyhold/v1/recovery/{repo_id}")))
}

pub fn check_config(config: &RepoConfig) -> Result<()> {
    if !READABLE_FORMATS.contains(&config.format) {
        return Err(Error::Format(format!(
            "this backup uses format {}, which this version cannot read",
            config.format
        )));
    }
    Ok(())
}

/// Unwraps the master key with the password or the recovery key.
pub fn unwrap_master_key(config: &RepoConfig, credential: &Credential) -> Result<Zeroizing<Vec<u8>>> {
    check_config(config)?;
    let aad = key_aad(&config.repo_id);
    for slot in &config.keys {
        let kek = match (credential, slot) {
            (Credential::Password(password), KeySlot::Known(KnownSlot::Password { kdf, params, salt, .. }))
                if kdf == "argon2id" =>
            {
                Some(password_kek(password, &from_base64(salt)?, params)?)
            }
            (Credential::RecoveryKey(text), KeySlot::Known(KnownSlot::Recovery { kdf, .. }))
                if kdf == "hkdf-sha256" =>
            {
                from_base32_groups(text, KEY_LENGTH).map(Zeroizing::new).map(|raw| recovery_kek(&raw, &config.repo_id))
            }
            _ => None,
        };
        let Some(kek) = kek else { continue };
        let wrapped = match slot {
            KeySlot::Known(KnownSlot::Password { wrapped_master_key, .. })
            | KeySlot::Known(KnownSlot::Recovery { wrapped_master_key, .. }) => from_base64(wrapped_master_key)?,
            KeySlot::Unknown(_) => continue,
        };
        if let Ok(master) = open(&kek, &wrapped, &aad) {
            return Ok(Zeroizing::new(master));
        }
    }
    Err(Error::WrongKey)
}

/// Opens a repository with the password or the recovery key.
pub fn unlock(config: &RepoConfig, credential: &Credential) -> Result<RepoKeys> {
    let master = unwrap_master_key(config, credential)?;
    Ok(RepoKeys::derive(&master, &config.repo_id))
}

fn password_slot(password: &str, master: &[u8], repo_id: &str, params: Argon2Params) -> Result<KeySlot> {
    let salt = random_bytes(32);
    let kek = password_kek(password, &salt, &params)?;
    Ok(KeySlot::Known(KnownSlot::Password {
        kdf: "argon2id".into(),
        params,
        salt: to_base64(&salt),
        wrapped_master_key: to_base64(&seal(&kek, master, &key_aad(repo_id), &random_array())),
    }))
}

/// A new repository: its `config`, its working keys and the recovery key
/// (shown to the user once, never stored).
pub struct NewRepository {
    pub config: RepoConfig,
    pub keys: RepoKeys,
    pub recovery_key: Zeroizing<String>,
}

/// Creates the keys and `config` of a new format 2 repository.
pub fn create_repository(password: &str, params: Argon2Params, created: String) -> Result<NewRepository> {
    let repo_id = random_id();
    let master = Zeroizing::new(random_bytes(KEY_LENGTH));
    let recovery = Zeroizing::new(random_bytes(KEY_LENGTH));
    let config = RepoConfig {
        format: WRITE_FORMAT,
        repo_id: repo_id.clone(),
        created,
        chunker: Chunker { kind: "fixed".into(), size: CHUNK_SIZE },
        keys: vec![
            password_slot(password, &master, &repo_id, params)?,
            KeySlot::Known(KnownSlot::Recovery {
                kdf: "hkdf-sha256".into(),
                wrapped_master_key: to_base64(&seal(
                    &recovery_kek(&recovery, &repo_id),
                    &master,
                    &key_aad(&repo_id),
                    &random_array(),
                )),
            }),
        ],
    };
    Ok(NewRepository {
        keys: RepoKeys::derive(&master, &repo_id),
        recovery_key: Zeroizing::new(to_base32_groups(&recovery)),
        config,
    })
}

/// Returns `config` with a new password. `credential` is the current
/// password, or the recovery key when the password was forgotten. The
/// recovery slot, unknown slots and all data are untouched.
pub fn change_password(
    config: &RepoConfig,
    credential: &Credential,
    new_password: &str,
    params: Argon2Params,
) -> Result<RepoConfig> {
    let master = unwrap_master_key(config, credential)?;
    let mut keys = vec![password_slot(new_password, &master, &config.repo_id, params)?];
    keys.extend(config.keys.iter().filter(|k| !matches!(k, KeySlot::Known(KnownSlot::Password { .. }))).cloned());
    Ok(RepoConfig { keys, ..config.clone() })
}
