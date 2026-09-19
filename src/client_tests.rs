//! Tests for building a client. None of them reads the process environment:
//! the builder is given a lookup of its own.

use std::ffi::OsString;

use super::*;
use crate::ErrorKind;

/// An environment with none of the SDK's variables.
fn empty(_: &str) -> Option<String> {
    None
}

/// The config error `builder` fails to build with, rendered.
fn config_error(builder: ClientBuilder) -> String {
    match builder.build_with_env(empty) {
        Ok(client) => panic!("expected a config error, built {client:?}"),
        Err(error) => {
            assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
            error.to_string()
        }
    }
}

/// A transport for `build_with_service`.
fn transport() -> HyperTransport {
    HyperTransport::new(TransportSettings {
        version: HttpVersion::Auto,
        extra_roots: Vec::new(),
        connect_timeout: None,
    })
    .expect("the transport builds")
}

#[test]
fn settings_the_builder_leaves_unset_come_from_the_environment() {
    let environment = |name: &str| match name {
        "TYPESAFE_API_KEY" => Some(OsString::from("env-key")),
        "TYPESAFE_BASE_URL" => Some(OsString::from("http://127.0.0.1:9/prefix/")),
        "TYPESAFE_DEFAULT_MODEL" => Some(OsString::from("env-model")),
        _ => None,
    };
    let client =
        Client::builder().build_with_env(environment).expect("the environment is complete");
    let shared = client.shared();

    assert_eq!(shared.config.endpoints().system_one(), "http://127.0.0.1:9/prefix/v1/systemone");
    assert_eq!(shared.config.default_model(), "env-model");
    assert_eq!(shared.post_headers["authorization"], "Bearer env-key");
    assert_eq!(&shared.model_json[..], b"\"env-model\"");
}

#[test]
fn the_default_version_is_http2_only_for_https_and_auto_for_http_and_a_choice_wins() {
    let rows = [
        ("https://127.0.0.1:9", None, HttpVersion::Http2Only),
        ("http://127.0.0.1:9", None, HttpVersion::Auto),
        ("https://127.0.0.1:9", Some(HttpVersion::Auto), HttpVersion::Auto),
        ("http://127.0.0.1:9", Some(HttpVersion::Http2Only), HttpVersion::Http2Only),
    ];
    for (base_url, chosen, expected) in rows {
        let mut builder = Client::builder().api_key("test-key").base_url(base_url);
        if let Some(version) = chosen {
            builder = builder.http_version(version);
        }
        let client = builder.build_with_env(empty).expect("it builds");
        let transport = format!("{:?}", client.shared().service);
        assert!(
            transport.contains(&format!("http_version: {expected:?},")),
            "{base_url} with {chosen:?}: {transport}"
        );
    }
}

#[test]
fn a_custom_transport_refuses_the_settings_only_the_default_one_has() {
    type Configure = fn(ClientBuilder) -> ClientBuilder;
    let rows: [(Configure, &str); 4] = [
        (|builder| builder.add_root_certificate(vec![1, 2, 3]), "add_root_certificate"),
        (|builder| builder.http_version(HttpVersion::Http2Only), "http_version"),
        (|builder| builder.connect_timeout(Duration::from_secs(1)), "connect_timeout"),
        (
            |builder| {
                builder
                    .connect_timeout(Duration::from_secs(1))
                    .add_root_certificate(vec![1])
                    .http_version(HttpVersion::Auto)
            },
            "add_root_certificate, http_version, connect_timeout",
        ),
    ];
    for (configure, named) in rows {
        let builder = configure(Client::builder().api_key("test-key"));
        let error = builder.build_with_service(transport()).expect_err("it must be refused");
        assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
        assert_eq!(
            error.to_string(),
            format!(
                "{named} configure the default transport, and a client built with \
                 build_with_service has a transport of its own."
            )
        );
    }

    // The same settings are accepted by the default transport.
    Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .http_version(HttpVersion::Http2Only)
        .connect_timeout(Duration::from_secs(1))
        .build_with_env(empty)
        .expect("the default transport takes them");
}

#[test]
fn a_default_header_that_cannot_be_sent_is_refused_without_its_value() {
    let secret = "sk-live-do-not-log";
    let rows = [
        (
            ("x team", "fine"),
            r#"The default header name "x team" is not a valid HTTP header name."#,
        ),
        (
            ("x-token", "sk-live-do-not-log\nInjected: yes"),
            r#"The value of the default header "x-token" is not a valid HTTP header value."#,
        ),
    ];
    for ((name, value), message) in rows {
        let rendered =
            config_error(Client::builder().api_key("test-key").default_header(name, value));
        assert_eq!(rendered, message);
        assert!(!rendered.contains(secret));
    }
}

#[test]
fn a_zero_connect_timeout_is_refused() {
    let rendered =
        config_error(Client::builder().api_key("test-key").connect_timeout(Duration::ZERO));
    assert_eq!(rendered, "connect_timeout must be a positive number of seconds.");
}

#[test]
fn a_later_default_header_of_a_name_replaces_an_earlier_one() {
    let client = Client::builder()
        .api_key("test-key")
        .default_header("X-Team", "first")
        .default_header("x-team", "second")
        .build_with_env(empty)
        .expect("it builds");
    let values: Vec<_> = client.shared().get_headers.get_all("x-team").iter().collect();
    assert_eq!(values, ["second"]);
}

// ---------------------------------------------------------------- Debug

const KEY: &str = "sk-live-0123456789-DO-NOT-LOG";

#[test]
fn a_builder_prints_what_was_set_and_nothing_secret() {
    let builder = Client::builder()
        .api_key(KEY)
        .base_url("https://example.test/prefix/")
        .default_model("jev-latest")
        .timeout(Duration::from_secs(3))
        .default_header("x-client-secret", "client-secret-value")
        .default_header("x-team", "team-value")
        .max_response_bytes(1024)
        .add_root_certificate(vec![0x30, 0x82])
        .http_version(HttpVersion::Auto)
        .connect_timeout(Duration::from_secs(1));

    assert_eq!(
        format!("{builder:?}"),
        concat!(
            "ClientBuilder { api_key: Some(<redacted>), base_url: Some([",
            r#""POST https://example.test/prefix/v1/systemone", "#,
            r#""GET https://example.test/prefix/v1/models"]), "#,
            r#"default_model: Some("jev-latest"), timeout: Some(Some(3s)), "#,
            r#"default_headers: ["x-client-secret", "x-team"], "#,
            "max_response_bytes: Some(1024), extra_roots: 1, http_version: Some(Auto), ",
            "connect_timeout: Some(1s) }",
        )
    );
    assert_eq!(
        format!("{:?}", ClientBuilder::default()),
        concat!(
            "ClientBuilder { api_key: None, base_url: None, default_model: None, timeout: None, ",
            "default_headers: [], max_response_bytes: None, extra_roots: 0, http_version: None, ",
            "connect_timeout: None }",
        )
    );
}

#[test]
fn a_builder_does_not_print_a_base_url_that_failed_its_checks() {
    for url in
        ["https://user:hunter2@example.test", "https://example.test/?token=hunter2", "not a url"]
    {
        let debug = format!("{:?}", Client::builder().base_url(url));
        assert!(debug.contains("base_url: Some(<not a usable URL>)"), "{url}: {debug}");
        assert!(!debug.contains("hunter2"), "{url}: {debug}");
    }
}

#[test]
fn a_client_prints_its_settings_and_transport_and_nothing_secret() {
    let client = Client::builder()
        .api_key(KEY)
        .base_url("https://example.test:443/prefix")
        .default_header("x-client-secret", "client-secret-value")
        .no_timeout()
        .build_with_env(empty)
        .expect("it builds");

    let debug = format!("{client:?}");
    assert_eq!(
        debug,
        concat!(
            r#"Client { config: Config { endpoints: ["POST https://example.test/prefix/v1/systemone", "#,
            r#""GET https://example.test/prefix/v1/models"], default_model: "jev-latest", "#,
            r#"timeout: None, max_response_bytes: 16777216, authorization: <redacted>, "#,
            r#"default_headers: ["x-client-secret"] }, "#,
            "transport: HyperTransport { http_version: Http2Only, extra_roots: 0, ",
            "connect_timeout: None } }",
        )
    );
    for secret in [KEY, "sk-live", "Bearer", "client-secret-value"] {
        assert!(!debug.contains(secret), "{secret} leaked into {debug}");
    }
}

#[test]
fn a_clone_shares_one_client() {
    let client = Client::builder().api_key("test-key").build_with_env(empty).expect("it builds");
    let clone = client.clone();
    assert!(std::ptr::eq(client.shared(), clone.shared()));
}
