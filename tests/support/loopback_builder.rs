// The start of a client of a loopback test server, for the targets that talk
// to one over each protocol. Each includes this file with `include!`, so it
// uses the names its includer imports.

/// A builder for a client of `server`: trusting its certificate when it has
/// one, and speaking prior-knowledge HTTP/2 to an h2c server.
fn loopback_builder(server: &TestServer, protocol: Protocol) -> ClientBuilder {
    let mut builder = Client::builder().api_key("test-key").base_url(server.base_url());
    if let Some(certificate) = server.certificate_der() {
        builder = builder.add_root_certificate(certificate.to_vec());
    }
    if protocol == Protocol::H2c {
        builder = builder.http_version(HttpVersion::Http2Only);
    }
    builder
}
