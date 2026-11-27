//! Publishing side: the release key (made once and sealed with its publisher's passphrase, [`seal`]; the test key
//! DPAPI-encrypted on the build PC; never in this repository) and the signed manifest for a release.

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};

use crate::{Manifest, hex, parse_version};

/// A new release key: its PKCS#8 document and its public half (hex, for [`crate::RELEASE_KEY`] or [`crate::TEST_KEY`]).
pub fn generate_key() -> Result<(Vec<u8>, String), String> {
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|e| format!("release key: {e}"))?;
    let public = public_key(document.as_ref())?;
    Ok((document.as_ref().to_vec(), public))
}

/// The start of a release key sealed with a passphrase ([`seal`]). The file: these 6 bytes, the PBKDF2-HMAC-SHA256
/// rounds (u32, big endian), a 16-byte salt and a 12-byte nonce, then the PKCS#8 document under AES-256-GCM with the
/// whole header authenticated. The security review of 2026-11-27: a key encrypted only for its Windows user (DPAPI)
/// could sign for any program running as that user; sealed, the file is worth nothing without its passphrase.
const SEALED: &[u8; 6] = b"QQRK1\n";
const HEADER: usize = 6 + 4 + 16 + 12;
/// Rounds for a newly sealed key: about a second on a desktop PC for each guess at the passphrase.
pub const SEAL_ROUNDS: u32 = 1_000_000;
/// The shortest passphrase a release key is sealed with.
pub const MIN_PASSPHRASE: usize = 12;

/// Whether `data` is a release key sealed with a passphrase (else: the test key, DPAPI-encrypted).
pub fn is_sealed(data: &[u8]) -> bool {
    data.starts_with(SEALED)
}

/// The release key (`pkcs8`) sealed with `passphrase` ([`SEAL_ROUNDS`]).
pub fn seal(pkcs8: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    seal_with_rounds(pkcs8, passphrase, SEAL_ROUNDS)
}

fn seal_with_rounds(pkcs8: &[u8], passphrase: &str, rounds: u32) -> Result<Vec<u8>, String> {
    use ring::aead::{Aad, Nonce};
    use ring::rand::SecureRandom;
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(format!(
            "the passphrase needs at least {MIN_PASSPHRASE} characters"
        ));
    }
    Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|e| format!("release key: {e}"))?;
    let random = SystemRandom::new();
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    random
        .fill(&mut salt)
        .and_then(|()| random.fill(&mut nonce))
        .map_err(|_| "no random numbers for the seal".to_string())?;
    let mut sealed = Vec::with_capacity(HEADER + pkcs8.len() + 16);
    sealed.extend_from_slice(SEALED);
    sealed.extend_from_slice(&rounds.to_be_bytes());
    sealed.extend_from_slice(&salt);
    sealed.extend_from_slice(&nonce);
    let mut body = pkcs8.to_vec();
    seal_key(passphrase, &salt, rounds)?
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(&sealed[..HEADER]),
            &mut body,
        )
        .map_err(|_| "sealing the release key failed".to_string())?;
    sealed.extend_from_slice(&body);
    Ok(sealed)
}

/// The release key (PKCS#8) in `sealed`, opened with `passphrase`.
pub fn open(sealed: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    use ring::aead::{Aad, Nonce};
    if !is_sealed(sealed) || sealed.len() < HEADER + 16 {
        return Err("not a release key sealed with a passphrase".into());
    }
    let rounds = u32::from_be_bytes([sealed[6], sealed[7], sealed[8], sealed[9]]);
    // A changed header is refused by the tag anyway; this only keeps a damaged file from taking hours.
    if rounds == 0 || rounds > 20 * SEAL_ROUNDS {
        return Err("the release key file is damaged (its rounds)".into());
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&sealed[26..HEADER]);
    let mut body = sealed[HEADER..].to_vec();
    let document = seal_key(passphrase, &sealed[10..26], rounds)?
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(&sealed[..HEADER]),
            &mut body,
        )
        .map(|plain| plain.to_vec())
        .map_err(|_| "wrong passphrase, or the release key file was changed".to_string());
    body.fill(0);
    let document = document?;
    Ed25519KeyPair::from_pkcs8(&document).map_err(|e| format!("release key: {e}"))?;
    Ok(document)
}

/// The AES-256-GCM key from the passphrase (PBKDF2-HMAC-SHA256).
fn seal_key(passphrase: &str, salt: &[u8], rounds: u32) -> Result<ring::aead::LessSafeKey, String> {
    let rounds = std::num::NonZeroU32::new(rounds).ok_or("no rounds")?;
    let mut bytes = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        rounds,
        salt,
        passphrase.as_bytes(),
        &mut bytes,
    );
    let key = ring::aead::UnboundKey::new(&ring::aead::AES_256_GCM, &bytes)
        .map_err(|_| "the seal's key".to_string());
    bytes.fill(0);
    Ok(ring::aead::LessSafeKey::new(key?))
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
    sign_manifest(pkcs8, &manifest)
}

/// The manifest for a Linux package built elsewhere (a .deb or an .rpm: its file name in the feed, its size and
/// SHA-256), so that only those reach the machine with the release key; its file name in the feed
/// ([`crate::MANIFEST_DEB`] or [`crate::MANIFEST_RPM`]), the manifest and its signature.
pub fn sign_package(
    pkcs8: &[u8],
    version: &str,
    package: &str,
    size: u64,
    sha256: &str,
    notes: &str,
) -> Result<(&'static str, Vec<u8>, String), String> {
    if parse_version(version).is_none() {
        return Err(format!("not major.minor.patch: {version}"));
    }
    let name = if package.ends_with(".deb") {
        crate::MANIFEST_DEB
    } else if package.ends_with(".rpm") {
        crate::MANIFEST_RPM
    } else {
        return Err(format!("{package}: not a .deb or an .rpm"));
    };
    if !crate::installer_names(version)[1..]
        .iter()
        .any(|name| name == package)
    {
        return Err(format!(
            "{package}: not a name a Linux package of {version} has ({})",
            crate::installer_names(version)[1..].join(", ")
        ));
    }
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!("{sha256}: not a SHA-256 in lower-case hex"));
    }
    if size == 0 || size > crate::MAX_INSTALLER {
        return Err(format!("{size} bytes: not a package's size"));
    }
    let manifest = Manifest {
        version: version.to_string(),
        installer: package.to_string(),
        size,
        sha256: sha256.to_string(),
        notes: notes.to_string(),
        published: crate::unix_now(),
    };
    let (bytes, signature) = sign_manifest(pkcs8, &manifest)?;
    Ok((name, bytes, signature))
}

fn sign_manifest(pkcs8: &[u8], manifest: &Manifest) -> Result<(Vec<u8>, String), String> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
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

    /// A sealed key opens with its passphrase only; any changed byte (the header too) is refused; the key is not in
    /// the file in the clear; short passphrases are refused (security review of 2026-11-27).
    #[test]
    fn a_sealed_key_opens_with_its_passphrase_only() {
        let (key, public) = generate_key().unwrap();
        let passphrase = "correct horse battery";
        let sealed = seal_with_rounds(&key, passphrase, 1000).unwrap();
        assert!(is_sealed(&sealed) && !is_sealed(&key));
        assert!(
            !sealed
                .windows(16)
                .any(|part| key.windows(16).any(|k| k == part))
        );
        let opened = open(&sealed, passphrase).unwrap();
        assert_eq!(public_key(&opened).unwrap(), public);
        assert!(open(&sealed, "correct horse batterY").is_err());
        assert!(open(&sealed, "").is_err());
        for at in [3, 9, 12, 30, HEADER + 5, sealed.len() - 1] {
            let mut changed = sealed.clone();
            changed[at] ^= 1;
            assert!(open(&changed, passphrase).is_err(), "byte {at} changed");
        }
        assert!(open(&sealed[..HEADER + 10], passphrase).is_err());
        assert!(seal_with_rounds(&key, "eleven char", 1000).is_err());
        assert!(seal_with_rounds(b"not a key", passphrase, 1000).is_err());
        // Two seals of one key differ (salt and nonce are new each time).
        assert_ne!(seal_with_rounds(&key, passphrase, 1000).unwrap(), sealed);
    }

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
        // A Linux package: signed from its name, size and SHA-256; checked against the package's bytes.
        let package = b"!<arch>\ndebian-binary the package".to_vec();
        let sha = hex(&Sha256::digest(&package));
        let (name, manifest, signature) = sign_package(
            &key,
            "0.2.0",
            "queqiao_0.2.0_amd64.deb",
            package.len() as u64,
            &sha,
            "",
        )
        .unwrap();
        assert!(sign_package(&key, "0.2.0", "queqiao_0.3.0_amd64.deb", 10, &sha, "").is_err());
        assert_eq!(name, crate::MANIFEST_DEB);
        let verified =
            crate::verify_manifest_with(&public_key(&key).unwrap(), &manifest, &signature).unwrap();
        assert_eq!(verified.installer, "queqiao_0.2.0_amd64.deb");
        assert!(crate::check_installer(&verified, &package).is_ok());
        assert!(sign_package(&key, "0.2.0", "../x.deb", 10, &sha, "").is_err());
        assert!(sign_package(&key, "0.2.0", "x.exe", 10, &sha, "").is_err());
        assert!(sign_package(&key, "0.2.0", "x.rpm", 10, "ABC", "").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip() {
        let sealed = protect(b"secret").unwrap();
        assert_ne!(sealed, b"secret");
        assert_eq!(unprotect(&sealed).unwrap(), b"secret");
    }
}
