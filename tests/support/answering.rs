// A server with one fixed answer, for the targets that need nothing more.
// Each includes this file with `include!`, so it uses the names its includer
// imports.

/// A server answering every request with `status` and the JSON `body`.
async fn answering(protocol: Protocol, status: StatusCode, body: impl AsRef<[u8]>) -> TestServer {
    let body = Bytes::copy_from_slice(body.as_ref());
    TestServer::start(protocol, move |_| {
        let body = body.clone();
        async move { json_response(status, body) }
    })
    .await
    .expect("the test server starts")
}
