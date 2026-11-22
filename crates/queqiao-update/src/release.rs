//! Publishing side: the release key (made once, kept DPAPI-encrypted for the person who publishes, never in this
//! repository) and the signed manifest for a release.

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};

use crate::{Manifest, hex, parse_version};

/// A new release key: its PKCS#8 document and its public half (hex, for [`crate::RELEASE_KEY`]).
pub fn generate_key() -> Result<(Vec<u8>, String), String> {
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|e| format!("release key: {e}"))?;
    let public = public_key(document.as_ref())?;
    Ok((document.as_ref().to_vec(), public))
}

/// The public half (hex) of a release key.
pub fn public_key(pkcs8: &[u8]) -> Result<String, String> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|e| format!("release key: {e}"))?;
    Ok(hex(pair.public_key().as_ref()))
}

/// The manifest for `installer` (its bytes) as `version`, and its signature (hex) by the release key.
pub fn sign(
    pkcs8: &[u8],
    version: &str,
    installer: &[u8],
    notes: &str,
) -> Result<(Vec<u8>, String), String> {
    if parse_version(version).is_none() {
        return Err(format!("not major.minor.patch: {version}"));
    }
    let manifest = Manifest {
        version: version.to_string(),
        installer: format!("queqiao-setup-{version}.exe"),
        size: installer.len() as u64,
        sha256: hex(&Sha256::digest(installer)),
        notes: notes.to_string(),
        published: crate::unix_now(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|e| format!("release key: {e}"))?;
    let signature = hex(pair.sign(&bytes).as_ref());
    Ok((bytes, signature))
}

#[cfg(windows)]
pub use dpapi::{protect, unprotect};

#[cfg(windows)]
mod dpapi {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr().cast_mut(),
        }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        let _ = unsafe { LocalFree(Some(HLOCAL(out.pbData.cast()))) };
        bytes
    }

    /// Encrypted for the person running this (Windows DPAPI).
    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &blob(data),
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|e| format!("DPAPI: {}", e.message()))?;
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(
                &blob(data),
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|e| format!("DPAPI: {}", e.message()))?;
        Ok(take(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_release_verifies_with_its_key_only() {
        let (key, public) = generate_key().unwrap();
        assert_eq!(public_key(&key).unwrap(), public);
        let installer = b"MZ installer bytes".to_vec();
        let (manifest, signature) = sign(&key, "0.2.0", &installer, "notes").unwrap();
        let verified = crate::verify_manifest_with(&public, &manifest, &signature).unwrap();
        assert_eq!(verified.installer, "queqiao-setup-0.2.0.exe");
        assert!(crate::check_installer(&verified, &installer).is_ok());
        let (_, other) = generate_key().unwrap();
        assert!(crate::verify_manifest_with(&other, &manifest, &signature).is_err());
        assert!(sign(&key, "0.2", &installer, "").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip() {
        let sealed = protect(b"secret").unwrap();
        assert_ne!(sealed, b"secret");
        assert_eq!(unprotect(&sealed).unwrap(), b"secret");
    }
}
