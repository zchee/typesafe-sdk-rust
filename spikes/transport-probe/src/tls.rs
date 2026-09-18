//! The rustls client configuration the SDK will build, in one place.
//!
//! Two things here are not incidental. The crypto provider is named explicitly
//! rather than taken from the process default, so a second provider entering
//! the dependency graph cannot make the choice ambiguous. And
//! `alpn_protocols` is left EMPTY, because
//! `hyper_rustls::HttpsConnectorBuilder::with_tls_config` asserts that it is
//! and aborts the process otherwise; hyper-rustls fills ALPN itself from
//! `enable_http1()` / `enable_http2()`.

use std::sync::Arc;

use rustls::{ClientConfig, crypto::CryptoProvider, pki_types::CertificateDer};
use rustls_platform_verifier::{BuilderVerifierExt as _, Verifier};

/// The aws-lc-rs provider, named rather than inherited.
#[must_use]
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// A client configuration trusting the operating system's roots, plus
/// `extra_roots`.
///
/// An empty `extra_roots` takes the plain platform verifier, which is the
/// default the SDK ships; a non-empty one takes
/// `Verifier::new_with_extra_roots`, which is what
/// `ClientBuilder::add_root_certificate` will call. The two paths are kept
/// side by side so the spike can show that extra roots ADD to the OS store
/// rather than replace it.
///
/// # Errors
///
/// Returns the rustls error if the verifier rejects a supplied root or the
/// provider cannot supply the default protocol versions.
pub fn client_config(
    extra_roots: Vec<CertificateDer<'static>>,
) -> Result<ClientConfig, rustls::Error> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()?;

    let config = if extra_roots.is_empty() {
        builder.with_platform_verifier()?.with_no_client_auth()
    } else {
        let verifier = Verifier::new_with_extra_roots(extra_roots, provider)?;
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth()
    };

    debug_assert!(config.alpn_protocols.is_empty(), "hyper-rustls asserts this");
    Ok(config)
}
