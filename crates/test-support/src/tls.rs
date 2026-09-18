//! Self-signed certificate and TLS acceptor for the HTTP/2-over-TLS server.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

use crate::Error;

/// Generates a fresh self-signed certificate for `127.0.0.1` and returns an
/// acceptor that offers only ALPN `h2`, together with the certificate in DER so
/// that a client can be told to trust it.
///
/// The subject alternative name is the **IP address** 127.0.0.1, not a DNS
/// name, because every URL this crate hands out is an IP literal: a DNS-name
/// SAN would not match and no second address would be raced during connect.
///
/// # Errors
///
/// Returns [`Error::Certificate`] when key or certificate generation fails, and
/// [`Error::TlsConfig`] when rustls rejects the generated pair.
pub(crate) fn self_signed_acceptor() -> Result<(TlsAcceptor, CertificateDer<'static>), Error> {
    // rcgen parses a SAN string that is a valid IP address into an IP SAN
    // rather than a DNS SAN, which is what a client connecting to
    // `https://127.0.0.1:PORT` checks against.
    let params =
        rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).map_err(Error::Certificate)?;
    let key_pair = rcgen::KeyPair::generate().map_err(Error::Certificate)?;
    let certificate = params.self_signed(&key_pair).map_err(Error::Certificate)?;
    let certificate_der = certificate.der().clone();
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

    // Naming the provider instead of relying on the process-wide default keeps
    // the acceptor independent of whatever the test binary installed first.
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(Error::TlsConfig)?
        .with_no_client_auth()
        .with_single_cert(vec![certificate_der.clone()], key_der)
        .map_err(Error::TlsConfig)?;
    // Offering only h2 means a client that cannot speak it fails ALPN
    // negotiation instead of silently falling back to HTTP/1.1.
    config.alpn_protocols = vec![b"h2".to_vec()];

    Ok((TlsAcceptor::from(Arc::new(config)), certificate_der))
}
