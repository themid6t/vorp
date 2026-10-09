use std::{path::PathBuf, sync::Arc};

use sha2::{Digest, Sha256};
use tokio_rustls::rustls::{
    self,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, pem::PemObject},
};

use crate::{
    RelayError,
    config::{RelayConfig, TlsConfig},
};

/// Holds the certificate used for new handshakes. Existing connections retain
/// the config they started with when files are reloaded.
pub(crate) struct TlsManager {
    current: Arc<rustls::ServerConfig>,
    files: Option<(PathBuf, PathBuf)>,
    fingerprint: Option<[u8; 32]>,
}

impl TlsManager {
    pub(crate) async fn new(config: &RelayConfig) -> Result<Self, RelayError> {
        match &config.tls {
            TlsConfig::SelfSigned { cert_output } => {
                // webpki rejects `*.localhost` (a wildcard over one label),
                // so the dashboard host is named on its own.
                let generated = rcgen::generate_simple_self_signed(vec![
                    config.base_domain.clone(),
                    config.dashboard_host.clone(),
                    format!("*.{}", config.base_domain),
                ])
                .map_err(|error| RelayError::Tls(format!("self-signed certificate: {error}")))?;
                tokio::fs::write(cert_output, generated.cert.pem())
                    .await
                    .map_err(|error| {
                        RelayError::Tls(format!(
                            "write development certificate {}: {error}",
                            cert_output.display()
                        ))
                    })?;
                let certs = vec![generated.cert.der().clone()];
                let key = PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()).into();
                Ok(Self {
                    current: configure(certs, key)?,
                    files: None,
                    fingerprint: None,
                })
            }
            TlsConfig::Files { cert, key } => {
                let (cert_bytes, key_bytes) = read_files(cert, key).await?;
                let current = parse_and_configure(&cert_bytes, &key_bytes)?;
                Ok(Self {
                    current,
                    files: Some((cert.clone(), key.clone())),
                    fingerprint: Some(fingerprint(&cert_bytes, &key_bytes)),
                })
            }
            TlsConfig::Acme(_) => Err(RelayError::Tls(
                "ACME certificate provisioning is not implemented yet".into(),
            )),
        }
    }

    pub(crate) fn acceptor(&self) -> tokio_rustls::TlsAcceptor {
        tokio_rustls::TlsAcceptor::from(Arc::clone(&self.current))
    }

    pub(crate) fn is_reloadable(&self) -> bool {
        self.files.is_some()
    }

    pub(crate) async fn reload_if_changed(&mut self) -> Result<bool, RelayError> {
        let Some((cert, key)) = &self.files else {
            return Ok(false);
        };
        let (cert_bytes, key_bytes) = read_files(cert, key).await?;
        let fingerprint = fingerprint(&cert_bytes, &key_bytes);
        if self.fingerprint == Some(fingerprint) {
            return Ok(false);
        }
        let replacement = parse_and_configure(&cert_bytes, &key_bytes)?;
        self.current = replacement;
        self.fingerprint = Some(fingerprint);
        Ok(true)
    }
}

async fn read_files(cert: &PathBuf, key: &PathBuf) -> Result<(Vec<u8>, Vec<u8>), RelayError> {
    let cert_bytes = tokio::fs::read(cert).await.map_err(|error| {
        RelayError::Tls(format!("read certificate {}: {error}", cert.display()))
    })?;
    let key_bytes = tokio::fs::read(key)
        .await
        .map_err(|error| RelayError::Tls(format!("read private key {}: {error}", key.display())))?;
    Ok((cert_bytes, key_bytes))
}

fn fingerprint(cert: &[u8], key: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update((cert.len() as u64).to_be_bytes());
    hash.update(cert);
    hash.update(key);
    hash.finalize().into()
}

fn parse_and_configure(
    cert_bytes: &[u8],
    key_bytes: &[u8],
) -> Result<Arc<rustls::ServerConfig>, RelayError> {
    let certs = CertificateDer::pem_slice_iter(cert_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RelayError::Tls(format!("parse certificate: {error}")))?;
    let key = PrivateKeyDer::from_pem_slice(key_bytes)
        .map_err(|error| RelayError::Tls(format!("parse private key: {error}")))?;
    configure(certs, key)
}

fn configure(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<rustls::ServerConfig>, RelayError> {
    // TLS 1.2 stays enabled for public tunnel visitors on older clients (Java 8,
    // older Android WebViews, corporate proxies); the agent negotiates 1.3.
    let mut server = rustls::ServerConfig::builder_with_protocol_versions(&[
        &rustls::version::TLS13,
        &rustls::version::TLS12,
    ])
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(|error| RelayError::Tls(format!("configure TLS certificate: {error}")))?;
    server.alpn_protocols = vec![
        vorp_protocol::AGENT_ALPN.to_vec(),
        b"h2".to_vec(),
        b"http/1.1".to_vec(),
    ];
    Ok(Arc::new(server))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_replacement_changes_fingerprint_and_invalid_key_is_rejected() {
        let first = rcgen::generate_simple_self_signed(vec!["example.test".into()])
            .expect("first test certificate");
        let second = rcgen::generate_simple_self_signed(vec!["example.test".into()])
            .expect("second test certificate");
        let first_cert = first.cert.pem();
        let first_key = first.signing_key.serialize_pem();
        let second_cert = second.cert.pem();
        let second_key = second.signing_key.serialize_pem();

        assert!(parse_and_configure(first_cert.as_bytes(), first_key.as_bytes()).is_ok());
        assert_ne!(
            fingerprint(first_cert.as_bytes(), first_key.as_bytes()),
            fingerprint(second_cert.as_bytes(), second_key.as_bytes())
        );
        assert!(parse_and_configure(second_cert.as_bytes(), second_key.as_bytes()).is_ok());
        assert!(parse_and_configure(second_cert.as_bytes(), b"invalid key").is_err());
    }
}
