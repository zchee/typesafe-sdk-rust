//! Tests for configuration resolution.
//!
//! The resolution cases are ports of the upstream Python SDK's
//! `tests/test_config.py` (`test_resolution`, `test_missing_key`,
//! `test_empty_env_unset`, `test_invalid_timeout`). Upstream sets real
//! environment variables through `monkeypatch`; here every case hands
//! [`Config::resolve`] its own lookup closure, so no test touches the process
//! environment and the cases can run in parallel.

use std::{cell::RefCell, error::Error as StdError};

use http::HeaderName;

use super::*;
use crate::ErrorKind;

// ------------------------------------------------------------- fixtures

/// The message a client with no API key fails with, as upstream words it.
const MISSING_KEY: &str =
    "No API key was provided. Pass api_key or set the TYPESAFE_API_KEY environment variable.";

/// A lookup that finds nothing: a process with none of the variables set.
fn no_env(_: &str) -> Option<String> {
    None
}

/// A lookup over a fixed set of variables.
fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_owned())
}

/// Resolves with only an explicit API key and the given environment.
fn with_key(lookup: impl Fn(&str) -> Option<String>) -> Config {
    Config::resolve(Explicit::default().api_key("test-key"), lookup)
        .unwrap_or_else(|error| panic!("a configuration with a key failed to resolve: {error:?}"))
}

/// Resolves and expects a configuration error, returning its rendering.
fn config_error(explicit: Explicit, lookup: impl Fn(&str) -> Option<String>) -> String {
    match Config::resolve(explicit, lookup) {
        Ok(config) => panic!("expected a configuration error, resolved {config:?}"),
        Err(error) => {
            assert!(
                matches!(error.kind(), ErrorKind::Config),
                "expected ErrorKind::Config, got {error:?}"
            );
            assert!(StdError::source(&error).is_none(), "a config error has no cause: {error:?}");
            error.to_string()
        }
    }
}

/// The `Debug` of a configuration error with this message, as `Error` renders
/// it.
fn config_debug(message: &str) -> String {
    format!("Error {{ kind: Config, message: {message:?} }}")
}

/// The `Authorization` value as text. It is flagged sensitive, which hides it
/// from `Debug` but not from the bytes.
fn authorization(config: &Config) -> &str {
    config.authorization().to_str().expect("invariant: the bearer value is ASCII")
}

// ------------------------------------------------------------ resolution

#[test]
fn defaults_apply_when_neither_the_caller_nor_the_environment_sets_anything() {
    let config = with_key(no_env);

    assert_eq!(authorization(&config), "Bearer test-key");
    assert_eq!(config.base_url(), "https://api.typesafe.ai");
    assert_eq!(config.endpoints().system_one(), "https://api.typesafe.ai/v1/systemone");
    assert_eq!(config.endpoints().models(), "https://api.typesafe.ai/v1/models");
    assert_eq!(config.default_model(), "jev-latest");
    assert_eq!(config.timeout(), Duration::from_secs(10));
    assert!(config.default_headers().is_empty());
}

/// Upstream `test_resolution`, all three sources, in one table.
#[test]
fn each_setting_comes_from_the_caller_then_the_environment_then_the_default() {
    let environment = [
        ("TYPESAFE_API_KEY", "  env-key  "),
        ("TYPESAFE_BASE_URL", "  https://env.test///  "),
        ("TYPESAFE_DEFAULT_MODEL", "  env-model  "),
    ];
    /// One source of settings and what resolving from it must produce.
    struct Case<'a> {
        source: &'a str,
        explicit: Explicit,
        environment: &'a [(&'a str, &'a str)],
        expected: [&'a str; 3],
    }
    let cases = [
        Case {
            source: "default",
            explicit: Explicit::default().api_key("test-key"),
            environment: &[],
            expected: ["Bearer test-key", "https://api.typesafe.ai/v1/systemone", "jev-latest"],
        },
        Case {
            source: "env",
            explicit: Explicit::default(),
            environment: &environment,
            expected: ["Bearer env-key", "https://env.test/v1/systemone", "env-model"],
        },
        Case {
            source: "constructor",
            explicit: Explicit::default()
                .api_key("code-key")
                .base_url("https://code.test///")
                .default_model("code-model"),
            environment: &environment,
            expected: ["Bearer code-key", "https://code.test/v1/systemone", "code-model"],
        },
    ];
    for Case { source, explicit, environment, expected: [key, url, model] } in cases {
        let config = Config::resolve(explicit, env(environment))
            .unwrap_or_else(|error| panic!("source {source}: {error:?}"));
        assert_eq!(authorization(&config), key, "source {source}");
        assert_eq!(config.endpoints().system_one(), url, "source {source}");
        assert_eq!(config.default_model(), model, "source {source}");
        assert_eq!(config.timeout(), Duration::from_secs(10), "source {source}");
    }
}

/// Upstream `test_missing_key`: unset, empty and whitespace-only all count as
/// no key.
#[test]
fn a_missing_or_blank_environment_key_is_the_upstream_error() {
    for value in [None, Some(""), Some(" \t\n ")] {
        let lookup = |name: &str| {
            assert_eq!(name, "TYPESAFE_API_KEY", "the key must be the first thing looked up");
            value.map(str::to_owned)
        };
        let error =
            Config::resolve(Explicit::default(), lookup).expect_err("a blank key must not resolve");
        assert!(matches!(error.kind(), ErrorKind::Config), "{value:?}: {error:?}");
        assert_eq!(error.to_string(), MISSING_KEY, "{value:?}");
        assert_eq!(format!("{error:?}"), config_debug(MISSING_KEY), "{value:?}");
    }
}

/// Upstream `test_empty_env_unset`. `TYPESAFE_LOG_LEVEL` is set as upstream
/// sets it, and is never even looked up: the SDK does not honour it.
#[test]
fn blank_environment_values_count_as_unset_and_the_log_level_is_never_read() {
    let looked_up = RefCell::new(Vec::new());
    let environment = [
        ("TYPESAFE_BASE_URL", " \t "),
        ("TYPESAFE_DEFAULT_MODEL", " \t "),
        ("TYPESAFE_LOG_LEVEL", " \t "),
    ];
    let lookup = |name: &str| {
        looked_up.borrow_mut().push(name.to_owned());
        env(&environment)(name)
    };

    let config = with_key(lookup);

    assert_eq!(config.endpoints().system_one(), "https://api.typesafe.ai/v1/systemone");
    assert_eq!(config.default_model(), "jev-latest");
    assert_eq!(*looked_up.borrow(), ["TYPESAFE_BASE_URL", "TYPESAFE_DEFAULT_MODEL"]);
}

#[test]
fn explicit_settings_are_used_without_consulting_the_environment() {
    let lookup = |name: &str| -> Option<String> {
        panic!("the environment was consulted for {name} although every setting was explicit")
    };
    let explicit = Explicit::default()
        .api_key(SecretString::from("code-key"))
        .base_url("https://code.test")
        .default_model("code-model")
        .timeout(Duration::from_millis(1500));

    let config = Config::resolve(explicit, lookup)
        .unwrap_or_else(|error| panic!("explicit settings failed to resolve: {error:?}"));

    assert_eq!(authorization(&config), "Bearer code-key");
    assert_eq!(config.base_url(), "https://code.test");
    assert_eq!(config.default_model(), "code-model");
    assert_eq!(config.timeout(), Duration::from_millis(1500));
}

/// Upstream `_resolve_env` returns an explicit value whenever it is not
/// `None`, so an explicit empty or whitespace-only key, or model, is kept
/// exactly as given rather than trimmed or treated as missing. This pins that
/// the port does the same: the key then fails at the server, not here.
#[test]
fn explicit_blank_values_are_kept_as_given() {
    let environment = [("TYPESAFE_API_KEY", "env-key"), ("TYPESAFE_DEFAULT_MODEL", "env-model")];
    let cases =
        [("", "Bearer ", ""), (" \t ", "Bearer  \t ", " \t "), ("  k  ", "Bearer   k  ", "  k  ")];
    for (given, header, model) in cases {
        let explicit = Explicit::default().api_key(given).default_model(given);
        let config = Config::resolve(explicit, env(&environment))
            .unwrap_or_else(|error| panic!("explicit {given:?} failed to resolve: {error:?}"));
        assert_eq!(authorization(&config), header, "explicit key {given:?}");
        assert_eq!(config.default_model(), model, "explicit model {given:?}");
    }
}

/// Python's `str.strip()` also strips U+001C to U+001F, which Rust's
/// `char::is_whitespace` does not; both strip U+00A0 and U+3000.
#[test]
fn environment_values_are_trimmed_as_python_trims_them() {
    let environment = [
        ("TYPESAFE_API_KEY", "\u{1c}\u{a0}env-key\u{3000}\u{1f}"),
        ("TYPESAFE_DEFAULT_MODEL", "\u{1d}\u{1e}"),
    ];

    let config = Config::resolve(Explicit::default(), env(&environment))
        .unwrap_or_else(|error| panic!("{error:?}"));

    assert_eq!(authorization(&config), "Bearer env-key");
    assert_eq!(config.default_model(), "jev-latest", "a value of separators only is blank");
}

#[test]
fn default_headers_are_kept_as_given_including_repeated_names() {
    let mut headers = HeaderMap::new();
    headers.insert("x-team", HeaderValue::from_static("billing"));
    headers.append("x-tag", HeaderValue::from_static("a"));
    headers.append("x-tag", HeaderValue::from_static("b"));

    let config = Config::resolve(
        Explicit::default().api_key("test-key").default_headers(headers.clone()),
        no_env,
    )
    .unwrap_or_else(|error| panic!("{error:?}"));

    assert_eq!(*config.default_headers(), headers);
    let tags: Vec<_> = config.default_headers().get_all("x-tag").iter().collect();
    assert_eq!(tags, ["a", "b"]);
}

// ------------------------------------------------------------- base URL

/// AC-F5's URL half: every trailing slash goes, and a path prefix survives.
#[test]
fn trailing_slashes_are_stripped_and_a_path_prefix_is_kept() {
    let config = Config::resolve(
        Explicit::default().api_key("test-key").base_url("https://example.test/prefix///"),
        no_env,
    )
    .unwrap_or_else(|error| panic!("{error:?}"));

    assert_eq!(config.base_url(), "https://example.test/prefix");
    assert_eq!(config.endpoints().system_one(), "https://example.test/prefix/v1/systemone");
    assert_eq!(config.endpoints().models(), "https://example.test/prefix/v1/models");
}

#[test]
fn endpoints_join_the_api_paths_onto_any_usable_base_url() {
    let cases = [
        ("https://api.typesafe.ai", "https://api.typesafe.ai/v1/systemone"),
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080/v1/systemone"),
        ("http://[::1]:8080/a/b", "http://[::1]:8080/a/b/v1/systemone"),
        ("https://example.test:8443/pre%20fix", "https://example.test:8443/pre%20fix/v1/systemone"),
        ("HTTPS://Example.TEST/Prefix", "https://Example.TEST/Prefix/v1/systemone"),
    ];
    for (base, expected) in cases {
        let joined = endpoints(base).unwrap_or_else(|error| panic!("{base}: {error:?}"));
        assert_eq!(joined.system_one(), expected, "base {base}");
        let models = expected.replace("/v1/systemone", "/v1/models");
        assert_eq!(joined.models(), models.as_str(), "base {base}");
    }
}

/// Each unusable base URL fails with its own message, and no message repeats
/// the URL: the userinfo cases carry a credential that must not reach a log.
#[test]
fn an_unusable_base_url_is_a_config_error_that_does_not_repeat_it() {
    let cases = [
        ("", "The base URL is not a valid URL."),
        (
            "https:",
            "The base URL must be absolute, with a scheme and a host, such as https://api.typesafe.ai.",
        ),
        (
            "example.test",
            "The base URL must be absolute, with a scheme and a host, such as https://api.typesafe.ai.",
        ),
        (
            "/v1",
            "The base URL must be absolute, with a scheme and a host, such as https://api.typesafe.ai.",
        ),
        ("https://exa mple.test", "The base URL is not a valid URL."),
        ("ftp://example.test", "The base URL must use http or https."),
        ("https://:443", "The base URL has an empty host."),
        ("https://example.test?key=sk-in-query", "The base URL must not carry a query ('?...')."),
        ("https://example.test/?", "The base URL must not carry a query ('?...')."),
        ("https://example.test#sk-in-fragment", "The base URL must not carry a fragment ('#...')."),
        (
            "https://user:sk-in-userinfo@example.test",
            "The base URL must not carry credentials; pass the API key on its own instead.",
        ),
        (
            "https://sk-in-userinfo@example.test",
            "The base URL must not carry credentials; pass the API key on its own instead.",
        ),
    ];
    for (base, message) in cases {
        let rendered = config_error(Explicit::default().api_key("test-key").base_url(base), no_env);
        assert_eq!(rendered, message, "base {base:?}");
        assert!(!rendered.contains("sk-"), "base {base:?} leaked into {rendered:?}");
    }
}

/// A base URL of slashes only is empty once they are stripped.
#[test]
fn a_base_url_of_slashes_only_is_not_a_url() {
    let rendered = config_error(Explicit::default().api_key("test-key").base_url("///"), no_env);
    assert_eq!(rendered, "The base URL is not a valid URL.");
}

#[test]
fn an_unusable_environment_base_url_fails_the_same_way() {
    let environment = [("TYPESAFE_BASE_URL", " https://user:sk-in-env@example.test/ ")];
    let rendered = config_error(Explicit::default().api_key("test-key"), env(&environment));
    assert_eq!(
        rendered,
        "The base URL must not carry credentials; pass the API key on its own instead."
    );
}

// -------------------------------------------------------------- timeout

/// Upstream `test_invalid_timeout` covers 0, -1, infinity and NaN. A
/// `Duration` cannot hold the last three, so zero is the case left.
#[test]
fn a_zero_timeout_is_the_upstream_error_and_any_positive_one_is_kept() {
    let message = "timeout must be a positive, finite number of seconds.";
    let rendered =
        config_error(Explicit::default().api_key("test-key").timeout(Duration::ZERO), no_env);
    assert_eq!(rendered, message);

    for timeout in [Duration::from_nanos(1), Duration::from_secs(7), Duration::MAX] {
        let config =
            Config::resolve(Explicit::default().api_key("test-key").timeout(timeout), no_env)
                .unwrap_or_else(|error| panic!("{timeout:?}: {error:?}"));
        assert_eq!(config.timeout(), timeout);
    }
}

// -------------------------------------------------------------- API key

#[test]
fn the_authorization_value_is_flagged_sensitive() {
    let config = with_key(no_env);
    assert!(config.authorization().is_sensitive());
    assert_eq!(format!("{:?}", config.authorization()), "Sensitive");
}

/// A key `http` could not put in a header, or that holds a character no API
/// key is spelled with, fails here with a message that does not repeat it.
#[test]
fn a_key_that_cannot_be_a_header_value_is_a_config_error_that_does_not_repeat_it() {
    let message = "The API key contains a character that cannot be sent in an HTTP header.";
    let cases = [
        "sk-secret\nInjected: header",
        "sk-secret\r",
        "sk-secret\u{0}",
        "sk-secret\u{7f}",
        "sk-secret\u{a0}",
        "sk-secret\u{201c}",
    ];
    for key in cases {
        let error = Config::resolve(Explicit::default().api_key(key), no_env)
            .expect_err("an unsendable key must not resolve");
        assert!(matches!(error.kind(), ErrorKind::Config), "{key:?}: {error:?}");
        assert_eq!(error.to_string(), message, "{key:?}");
        let debug = format!("{error:?}");
        assert_eq!(debug, config_debug(message), "{key:?}");
        assert!(!debug.contains("sk-secret"), "{key:?} leaked into {debug}");
    }
}

/// Every printable ASCII character, a space and a tab are accepted as they
/// are.
#[test]
fn a_key_of_printable_ascii_is_sent_byte_for_byte() {
    let key: String = (b' '..=b'~').map(char::from).chain(['\t']).collect();
    let config = Config::resolve(Explicit::default().api_key(key.as_str()), no_env)
        .unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(authorization(&config), format!("Bearer {key}"));
}

/// An environment key that is not a valid header value is refused the same
/// way as an explicit one.
#[test]
fn an_unsendable_environment_key_is_refused_without_repeating_it() {
    let environment = [("TYPESAFE_API_KEY", "sk-env\u{7f}secret")];
    let rendered = config_error(Explicit::default(), env(&environment));
    assert_eq!(rendered, "The API key contains a character that cannot be sent in an HTTP header.");
}

// ---------------------------------------------------------------- Debug

#[test]
fn debug_prints_neither_the_key_nor_any_default_header_value() {
    let key = "sk-live-0123456789abcdefDO-NOT-LOG";
    let mut headers = HeaderMap::new();
    headers.insert("x-team", HeaderValue::from_static("billing-team-value"));
    headers.insert(
        HeaderName::from_static("x-client-secret"),
        HeaderValue::from_static("client-secret-value"),
    );
    headers.append("x-team", HeaderValue::from_static("second-team-value"));
    let explicit = Explicit::default()
        .api_key(key)
        .base_url("https://example.test/prefix/")
        .default_model("jev-latest")
        .default_headers(headers);

    let config = Config::resolve(explicit, no_env).unwrap_or_else(|error| panic!("{error:?}"));
    let debug = format!("{config:?}");

    assert_eq!(
        debug,
        concat!(
            r#"Config { base_url: "https://example.test/prefix", default_model: "jev-latest", "#,
            r#"timeout: 10s, authorization: <redacted>, "#,
            r#"default_headers: ["x-team", "x-client-secret"] }"#,
        )
    );
    for secret in
        [key, "sk-live", "Bearer", "billing-team-value", "second-team-value", "client-secret-value"]
    {
        assert!(!debug.contains(secret), "{secret:?} leaked into {debug}");
    }
}
