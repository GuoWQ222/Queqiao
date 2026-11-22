//! The server's TLS certificate. On first start a self-signed certificate is made for the given names and
//! kept in the data folder; clients pin its SHA-256 fingerprint (printed at start). A certificate from a
//! public CA can be put in its place (server.crt / server.key, PEM).

use std::path::Path;
use std::sync::Arc;

use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::store::sha256_hex;

/// The TLS set-up and the certificate's SHA-256 fingerprint (hex).
pub fn load_or_create(
    data: &Path,
    names: &[String],
) -> Result<(Arc<ServerConfig>, String), String> {
    let cert_path = data.join("server.crt");
    let key_path = data.join("server.key");
    if !cert_path.exists() || !key_path.exists() {
        let mut subject_names = names.to_vec();
        subject_names.push("localhost".into());
        let generated =
            rcgen::generate_simple_self_signed(subject_names).map_err(|e| e.to_string())?;
        std::fs::write(&cert_path, generated.cert.pem()).map_err(|e| e.to_string())?;
        std::fs::write(&key_path, generated.signing_key.serialize_pem())
            .map_err(|e| e.to_string())?;
        // Its owner's only on Linux, as the data folder is (the security review of 2026-11-22).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
    }
    let cert = CertificateDer::from_pem_file(&cert_path)
        .map_err(|e| format!("{}: {e}", cert_path.display()))?;
    let key = PrivateKeyDer::from_pem_file(&key_path)
        .map_err(|e| format!("{}: {e}", key_path.display()))?;
    let fingerprint = sha256_hex(cert.as_ref());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|e| e.to_string())?;
    Ok((Arc::new(config), fingerprint))
}
