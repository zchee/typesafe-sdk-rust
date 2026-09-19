//! Tests for the default transport's construction and error mapping.

use std::io;

use super::*;
use crate::ErrorKind;

fn settings(version: HttpVersion) -> TransportSettings {
    TransportSettings { version, extra_roots: Vec::new(), connect_timeout: None }
}

#[test]
fn the_transport_prints_its_settings_and_counts_its_roots() {
    let transport = HyperTransport::new(settings(HttpVersion::Http2Only)).expect("it builds");
    assert_eq!(
        format!("{transport:?}"),
        "HyperTransport { http_version: Http2Only, extra_roots: 0, connect_timeout: None }"
    );

    let transport = HyperTransport::new(TransportSettings {
        version: HttpVersion::Auto,
        extra_roots: Vec::new(),
        connect_timeout: Some(Duration::from_millis(1500)),
    })
    .expect("it builds");
    assert_eq!(
        format!("{transport:?}"),
        "HyperTransport { http_version: Auto, extra_roots: 0, connect_timeout: Some(1.5s) }"
    );
}

#[test]
fn a_root_that_is_not_a_certificate_is_a_config_error() {
    let garbage = b"-----BEGIN CERTIFICATE----- not DER at all".to_vec();
    // What the platform verifier says about these bytes differs by operating
    // system, so the expected message is built from its own answer.
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let refusal = Verifier::new_with_extra_roots([CertificateDer::from(garbage.clone())], provider)
        .expect_err("the bytes are not a certificate");

    let error = HyperTransport::new(TransportSettings {
        version: HttpVersion::Http2Only,
        extra_roots: vec![garbage],
        connect_timeout: None,
    })
    .expect_err("a bad root must not build");

    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    assert_eq!(
        error.to_string(),
        format!("The TLS certificate verifier could not be built: {refusal}.")
    );
}

#[test]
fn a_timed_out_connect_is_the_connect_timeout() {
    let expired = io::Error::new(io::ErrorKind::TimedOut, "connect timeout");
    let refused = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
    let wrapped: Box<dyn StdError + Send + Sync> = Box::new(io::Error::other(Chained(expired)));

    assert!(timed_out(&Chained(io::Error::new(io::ErrorKind::TimedOut, "t"))));
    assert!(timed_out(&*wrapped), "found below another I/O error");
    assert!(!timed_out(&refused));
    assert!(!timed_out(&Chained(refused)));
}

/// An error whose cause is an I/O error, as a connector reports one.
#[derive(Debug)]
struct Chained(io::Error);

impl fmt::Display for Chained {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("tcp connect error")
    }
}

impl StdError for Chained {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.0)
    }
}
