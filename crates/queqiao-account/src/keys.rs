//! The device's own Ed25519 key, and the keys of the account's other devices as this PC first saw them.
//!
//! - `device_key.json` in `%APPDATA%\Queqiao`: the private key (PKCS#8), DPAPI-encrypted for the Windows user,
//!   shared by the device's agent and viewer.
//! - `known_devices.json`: device id -> public key, pinned on first contact (as SSH does with host keys). A
//!   device whose key later differs is refused until the pin is removed on purpose (`forget`): the server, or
//!   someone who took it over, cannot slip in its own key.

use std::collections::BTreeMap;
use std::sync::Arc;

use queqiao_transport::Error;
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

use crate::config::{folder, hex, protect, unhex, unprotect};
use crate::tls::err;

#[derive(Clone)]
pub struct DeviceKey {
    pair: Arc<Ed25519KeyPair>,
    /// The private key as stored (PKCS#8), for the service's agent at the sign-in screen.
    pkcs8: Arc<Vec<u8>>,
    public_hex: String,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceKey({})", self.public_hex)
    }
}

impl DeviceKey {
    /// This PC's key: made once, then reused.
    pub fn load_or_create() -> Result<DeviceKey, Error> {
        let path = folder()?.join("device_key.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let sealed: String = serde_json::from_str(&text).map_err(err)?;
            let pkcs8 =
                unprotect(&unhex(&sealed).ok_or_else(|| Error("damaged device key file".into()))?)?;
            return DeviceKey::from_pkcs8(&pkcs8);
        }
        let pkcs8 = generate_pkcs8()?;
        let key = DeviceKey::from_pkcs8(&pkcs8)?;
        let sealed = hex(&protect(&pkcs8)?);
        // The agent and the viewer may start together: the first file written wins.
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(serde_json::to_string(&sealed)?.as_bytes())
            });
        match created {
            Ok(()) => Ok(key),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                DeviceKey::load_or_create()
            }
            Err(error) => Err(error.into()),
        }
    }

    /// A key that lives only in memory (tests, probes).
    pub fn generate() -> Result<DeviceKey, Error> {
        DeviceKey::from_pkcs8(&generate_pkcs8()?)
    }

    /// A key from its PKCS#8 bytes (the agent at the sign-in screen gets them from the service).
    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<DeviceKey, Error> {
        let pair =
            Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|e| Error(format!("device key: {e}")))?;
        let public_hex = hex(pair.public_key().as_ref());
        Ok(DeviceKey {
            pair: Arc::new(pair),
            pkcs8: Arc::new(pkcs8.to_vec()),
            public_hex,
        })
    }

    /// The private key (PKCS#8): only for handing this device to the Queqiao service.
    pub fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    /// The public key, hex (what the server and the other devices see).
    pub fn public_hex(&self) -> &str {
        &self.public_hex
    }

    pub fn sign(&self, message: &[u8]) -> String {
        hex(self.pair.sign(message).as_ref())
    }
}

fn generate_pkcs8() -> Result<Vec<u8>, Error> {
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|e| Error(format!("device key: {e}")))?;
    Ok(document.as_ref().to_vec())
}

/// True if `signature` (hex) is `public_key`'s (hex) signature over `message`.
pub fn verify(public_key: &str, message: &[u8], signature: &str) -> bool {
    match (unhex(public_key), unhex(signature)) {
        (Some(key), Some(signature)) => UnparsedPublicKey::new(&ED25519, key)
            .verify(message, &signature)
            .is_ok(),
        _ => false,
    }
}

/// The keys of other devices as first seen from this PC.
pub struct KnownDevices {
    keys: BTreeMap<String, String>,
    /// None: in memory only (tests, probes).
    path: Option<std::path::PathBuf>,
}

/// What a device's key is compared with its pin.
#[derive(Debug, PartialEq, Eq)]
pub enum Pin {
    /// Seen for the first time: now pinned.
    New,
    Same,
    /// Different from the pinned key (hex): refused.
    Changed(String),
}

impl KnownDevices {
    pub fn load() -> Result<KnownDevices, Error> {
        let path = folder()?.join("known_devices.json");
        let keys = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Ok(KnownDevices {
            keys,
            path: Some(path),
        })
    }

    pub fn in_memory() -> KnownDevices {
        KnownDevices::with_pins(Vec::new())
    }

    /// In memory, starting from these pins (device id, public key hex).
    pub fn with_pins(pins: Vec<(String, String)>) -> KnownDevices {
        KnownDevices {
            keys: pins
                .into_iter()
                .map(|(device, key)| (device, key.to_ascii_lowercase()))
                .collect(),
            path: None,
        }
    }

    /// All pins (device id, public key hex).
    pub fn pins(&self) -> Vec<(String, String)> {
        self.keys
            .iter()
            .map(|(device, key)| (device.clone(), key.clone()))
            .collect()
    }

    /// Compares `public_key` with the pin for `device`, pinning it the first time.
    pub fn check(&mut self, device: &str, public_key: &str) -> Pin {
        let key = public_key.to_ascii_lowercase();
        match self.keys.get(device) {
            Some(pinned) if *pinned == key => Pin::Same,
            Some(pinned) => Pin::Changed(pinned.clone()),
            None => {
                self.keys.insert(device.to_string(), key);
                let _ = self.save();
                Pin::New
            }
        }
    }

    /// Removes the pin of `device` (the person confirmed its new key); the next contact pins again.
    pub fn forget(&mut self, device: &str) -> Result<(), Error> {
        self.keys.remove(device);
        self.save()
    }

    fn save(&self) -> Result<(), Error> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        // Written whole to a temporary file and renamed: a half-written pin file would lose pins.
        let temporary = path.with_extension("json.tmp");
        std::fs::write(
            &temporary,
            serde_json::to_string_pretty(&self.keys).map_err(err)?,
        )?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_and_pins() {
        let key = DeviceKey::generate().unwrap();
        let other = DeviceKey::generate().unwrap();
        let signature = key.sign(b"offer");
        assert!(verify(key.public_hex(), b"offer", &signature));
        assert!(!verify(
            key.public_hex(),
            b"offer with another fingerprint",
            &signature
        ));
        assert!(!verify(other.public_hex(), b"offer", &signature));
        assert!(!verify(key.public_hex(), b"offer", "00"));

        let mut known = KnownDevices::in_memory();
        assert_eq!(known.check("dev", key.public_hex()), Pin::New);
        assert_eq!(
            known.check("dev", &key.public_hex().to_uppercase()),
            Pin::Same
        );
        assert_eq!(
            known.check("dev", other.public_hex()),
            Pin::Changed(key.public_hex().to_string())
        );
        known.forget("dev").unwrap();
        assert_eq!(known.check("dev", other.public_hex()), Pin::New);
    }
}
