use std::sync::Arc;

use tokio_rustls::rustls::{
    self,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, pem::PemObject},
};

use crate::{
    RelayError,
    config::{RelayConfig, TlsConfig},
};

pub(crate) async fn acceptor(
    config: &RelayConfig,
) -> Result<tokio_rustls::TlsAcceptor, RelayError> {
    let (certs, key) = match &config.tls {
        TlsConfig::SelfSigned { cert_output } => {
            let generated = rcgen::generate_simple_self_signed(vec![
                config.base_domain.clone(),
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
            (certs, key)
        }
        TlsConfig::Files { cert, key } => {
            let cert_bytes = tokio::fs::read(cert).await.map_err(|error| {
                RelayError::Tls(format!("read certificate {}: {error}", cert.display()))
            })?;
            let key_bytes = tokio::fs::read(key).await.map_err(|error| {
                RelayError::Tls(format!("read private key {}: {error}", key.display()))
            })?;
            let certs = CertificateDer::pem_slice_iter(&cert_bytes)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| RelayError::Tls(format!("parse certificate: {error}")))?;
            let key = PrivateKeyDer::from_pem_slice(&key_bytes)
                .map_err(|error| RelayError::Tls(format!("parse private key: {error}")))?;
            (certs, key)
        }
        TlsConfig::Acme(_) => {
            return Err(RelayError::Tls(
                "ACME certificate provisioning is not implemented yet".into(),
            ));
        }
    };
    let mut server =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|error| RelayError::Tls(format!("configure TLS certificate: {error}")))?;
    server.alpn_protocols = vec![
        vorp_protocol::AGENT_ALPN.to_vec(),
        b"h2".to_vec(),
        b"http/1.1".to_vec(),
    ];
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(server)))
}
