//! Where a client's settings come from, and what makes them usable.
//!
//! Resolution is a pure function over a lookup closure rather than a reader of
//! the process environment, for two reasons: `std::env::set_var` is `unsafe` in
//! edition 2024, so a test that wanted to set a variable could not do it
//! without breaking the crate's `#![forbid(unsafe_code)]`; and the process
//! environment is global mutable state that two tests running in parallel
//! would fight over. The closure makes each case its own input.
//!
//! An explicit setting wins over the environment, an environment value is
//! trimmed, a blank one counts as unset, a trailing slash comes off the base
//! URL, and a deadline must be above zero when there is one.

use std::{ffi::OsString, fmt, time::Duration};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Uri, uri::PathAndQuery};
use secrecy::{ExposeSecret, SecretString};

use crate::{
    constants::{
        API_KEY_ENV, BASE_URL_ENV, DEFAULT_BASE_URL, DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_MODEL,
        DEFAULT_MODEL_ENV, DEFAULT_TIMEOUT, MODELS_PATH, SYSTEM_ONE_PATH,
    },
    error::{Error, format_endpoint},
};

/// The settings a caller passed explicitly. Anything left unset falls back to
/// its environment variable, then to the SDK's default.
#[derive(Default)]
pub(crate) struct Explicit {
    api_key: Option<SecretString>,
    base_url: Option<String>,
    default_model: Option<String>,
    /// `None` leaves the default; `Some(None)` asks for no deadline at all.
    timeout: Option<Option<Duration>>,
    default_headers: HeaderMap,
    max_response_bytes: Option<usize>,
}

impl Explicit {
    /// The API key, used as given: an explicit key is not trimmed, and a blank
    /// one is refused when the configuration is resolved.
    pub(crate) fn api_key(mut self, key: impl Into<SecretString>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// The API root, used as given apart from losing its trailing slashes.
    pub(crate) fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// The model a request names when the call names none, used as given; a
    /// blank one is refused when the configuration is resolved.
    pub(crate) fn default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = Some(model.into());
        self
    }

    /// The deadline of each attempt.
    pub(crate) fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(Some(timeout));
        self
    }

    /// No deadline on any attempt.
    pub(crate) fn no_timeout(mut self) -> Self {
        self.timeout = Some(None);
        self
    }

    /// The largest response body a request reads.
    pub(crate) fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = Some(limit);
        self
    }

    /// Headers sent on every request unless a call or the SDK overrides them.
    pub(crate) fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers = headers;
        self
    }
}

/// A client's settings once every source has been consulted and every value
/// checked.
///
/// The API key survives only as the `Authorization` value built from it,
/// flagged sensitive, and `Debug` prints neither that value nor the values of
/// the default headers.
pub(crate) struct Config {
    authorization: HeaderValue,
    endpoints: Endpoints,
    default_model: Box<str>,
    timeout: Option<Duration>,
    default_headers: HeaderMap,
    max_response_bytes: usize,
}

impl Config {
    /// Resolves the settings from what the caller passed and what `env` finds.
    ///
    /// `env` looks a variable up by name; the client passes
    /// `std::env::var_os`. It is consulted only for settings the caller left
    /// unset. A value it returns is trimmed, and a blank one counts as unset;
    /// one that is not UTF-8 is refused, by the name of the variable. An
    /// explicit value always wins and is taken as given, without trimming, as
    /// the Python SDK takes it - except that an explicit API key or default
    /// model that is blank is refused rather than sent, where the Python SDK
    /// would send it.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when no
    /// API key is found, when an explicit API key or default model is blank,
    /// when the key cannot be sent in an HTTP header, when the base URL is not
    /// an absolute `http` or `https` URL without credentials, query or
    /// fragment, when the timeout or the response size limit is zero, or when
    /// a variable `env` reads is not UTF-8.
    pub(crate) fn resolve<V>(
        explicit: Explicit,
        env: impl Fn(&str) -> Option<V>,
    ) -> Result<Self, Error>
    where
        V: Into<OsString>,
    {
        let Explicit {
            api_key,
            base_url,
            default_model,
            timeout,
            default_headers,
            max_response_bytes,
        } = explicit;

        let api_key = match api_key {
            // A blank key can only be a mistake, and sending it would turn a
            // configuration error into an authentication failure at the
            // server. The message does not repeat the value: whitespace is
            // still part of what the caller passed as a credential.
            Some(key) if is_blank(key.expose_secret()) => {
                return Err(Error::config(format!(
                    "The API key is empty. \
                     Pass a non-empty api_key or set the {API_KEY_ENV} environment variable."
                )));
            }
            Some(key) => key,
            None => from_env(&env, API_KEY_ENV)?.map(SecretString::from).ok_or_else(|| {
                Error::config(format!(
                    "No API key was provided. \
                     Pass api_key or set the {API_KEY_ENV} environment variable."
                ))
            })?,
        };
        let authorization = bearer(&api_key)?;

        let base_url = match base_url {
            Some(url) => Some(url),
            None => from_env(&env, BASE_URL_ENV)?,
        };
        let mut base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        base_url.truncate(base_url.trim_end_matches('/').len());
        let endpoints = endpoints(&base_url)?;

        let default_model = match default_model {
            Some(model) if is_blank(&model) => {
                return Err(Error::config(format!(
                    "The default model is empty. \
                     Pass a non-empty default_model or set the {DEFAULT_MODEL_ENV} \
                     environment variable."
                )));
            }
            Some(model) => model,
            None => from_env(&env, DEFAULT_MODEL_ENV)?.unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        };

        let timeout = timeout.unwrap_or(Some(DEFAULT_TIMEOUT));
        // A `Duration` cannot be negative, NaN or infinite, so zero is the one
        // unusable deadline left to refuse. No deadline at all is asked for
        // by name, never by a sentinel value.
        if timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(Error::config(ZERO_TIMEOUT));
        }

        let max_response_bytes = max_response_bytes.unwrap_or(DEFAULT_MAX_RESPONSE_BYTES);
        if max_response_bytes == 0 {
            return Err(Error::config(
                "max_response_bytes must be at least 1: every response carries a body.",
            ));
        }

        Ok(Self {
            authorization,
            endpoints,
            default_model: default_model.into_boxed_str(),
            timeout,
            default_headers,
            max_response_bytes,
        })
    }

    /// The `Authorization: Bearer <key>` value, flagged sensitive so that
    /// `http` and `hyper` keep it out of their own `Debug` output and out of
    /// HTTP/2 header compression tables.
    pub(crate) fn authorization(&self) -> &HeaderValue {
        &self.authorization
    }

    /// The two endpoint URLs, parsed once.
    pub(crate) fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// The model a request names when the call names none.
    pub(crate) fn default_model(&self) -> &str {
        &self.default_model
    }

    /// The deadline of each attempt, or `None` for no deadline.
    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// The largest response body a request reads, in bytes.
    pub(crate) fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    /// The caller's default headers, as given.
    pub(crate) fn default_headers(&self) -> &HeaderMap {
        &self.default_headers
    }
}

impl fmt::Debug for Config {
    /// Prints the settings a reader needs to tell two clients apart, and no
    /// value that could be a credential: the `Authorization` value is replaced
    /// by a marker and the default headers are reduced to their names, since a
    /// caller may pass a token there under any name at all. The endpoints are
    /// printed as an error names them, which is the base URL without a default
    /// port; a base URL can carry no userinfo, query or fragment, but it keeps
    /// its path, so a credential put into that path would show here.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("endpoints", &self.endpoints)
            .field("default_model", &self.default_model)
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("authorization", &Hidden)
            .field("default_headers", &HeaderNames(&self.default_headers))
            .finish()
    }
}

/// Stands in for a credential in a `Debug`.
struct Hidden;

impl fmt::Debug for Hidden {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Stands in for a header map in a `Debug`: the names, once each, in the order
/// they were first inserted.
struct HeaderNames<'a>(&'a HeaderMap);

impl fmt::Debug for HeaderNames<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.0.keys()).finish()
    }
}

/// The URLs of the two API endpoints under one base URL.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Endpoints {
    system_one: Uri,
    models: Uri,
}

impl fmt::Debug for Endpoints {
    /// `["POST <system one URL>", "GET <models URL>"]`, each as an error names
    /// the endpoint it failed at.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_list()
            .entry(&format_endpoint(&Method::POST, &self.system_one))
            .entry(&format_endpoint(&Method::GET, &self.models))
            .finish()
    }
}

impl Endpoints {
    /// The System One endpoint, `<base>/v1/systemone`.
    pub(crate) fn system_one(&self) -> &Uri {
        &self.system_one
    }

    /// The model listing endpoint, `<base>/v1/models`.
    pub(crate) fn models(&self) -> &Uri {
        &self.models
    }
}

/// Joins the API paths onto `base_url`, which must already have lost its
/// trailing slashes.
///
/// The base URL may carry a path prefix, which the API paths are appended to.
/// None of the error messages repeats the URL: its userinfo, when it has any,
/// is a credential, and a URL that failed to parse cannot be trusted to have
/// had its userinfo found and cut out.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
/// `base_url` does not parse, is not absolute, is not `http` or `https`, has an
/// empty host, or carries userinfo, a query or a fragment.
pub(crate) fn endpoints(base_url: &str) -> Result<Endpoints, Error> {
    // `http::Uri` drops a fragment without a word when it parses, so the only
    // place left to see one is the text. The first `#` or `?` of a URL always
    // starts the fragment or the query, so a plain search is exact.
    if base_url.contains('#') {
        return Err(Error::config("The base URL must not carry a fragment ('#...')."));
    }
    if base_url.contains('?') {
        return Err(Error::config("The base URL must not carry a query ('?...')."));
    }
    let base: Uri =
        base_url.parse().map_err(|_| Error::config("The base URL is not a valid URL."))?;
    let (Some(scheme), Some(authority)) = (base.scheme_str(), base.authority()) else {
        return Err(Error::config(
            "The base URL must be absolute, with a scheme and a host, \
             such as https://api.typesafe.ai.",
        ));
    };
    if !matches!(scheme, "http" | "https") {
        return Err(Error::config("The base URL must use http or https."));
    }
    if authority.as_str().contains('@') {
        return Err(Error::config(
            "The base URL must not carry credentials; pass the API key on its own instead.",
        ));
    }
    if authority.host().is_empty() {
        return Err(Error::config("The base URL has an empty host."));
    }

    // `http` gives a URL with no path the path `/`, which would double the
    // slash in front of the API path; a real prefix has no trailing slash left.
    let prefix = base.path().trim_end_matches('/');
    let join = |path: &str| {
        let mut parts = base.clone().into_parts();
        parts.path_and_query = Some(
            PathAndQuery::from_maybe_shared(Bytes::from(format!("{prefix}{path}")))
                .expect("invariant: a parsed path followed by a fixed API path is a valid path"),
        );
        Uri::from_parts(parts).expect(
            "invariant: the scheme and authority were checked present, and the path is valid",
        )
    };
    Ok(Endpoints { system_one: join(SYSTEM_ONE_PATH), models: join(MODELS_PATH) })
}

/// Builds the `Authorization` value for `key`, flagged sensitive.
///
/// This is the one place the key is read out of its secret wrapper. The value
/// is assembled in a buffer that becomes the header's own storage, so no
/// second copy of the key is left behind in freed memory.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error, which does
/// not repeat the key, when the key holds anything but printable ASCII, spaces
/// and tabs.
fn bearer(key: &SecretString) -> Result<HeaderValue, Error> {
    const SCHEME: &[u8] = b"Bearer ";
    let key = key.expose_secret().as_bytes();
    // `http` would also accept bytes above 0x7F, but no API key is spelled
    // with them: one there is a paste of a curly quote or a non-breaking space,
    // and sending it would only fail later as an authentication error.
    if !key.iter().all(|&byte| byte == b'\t' || (b' '..=b'~').contains(&byte)) {
        return Err(Error::config(
            "The API key contains a character that cannot be sent in an HTTP header.",
        ));
    }
    let mut value = Vec::with_capacity(SCHEME.len() + key.len());
    value.extend_from_slice(SCHEME);
    value.extend_from_slice(key);
    let mut value = HeaderValue::from_maybe_shared(Bytes::from(value))
        .expect("invariant: every byte was checked to be printable ASCII, a space or a tab");
    value.set_sensitive(true);
    Ok(value)
}

/// The message a zero deadline is refused with, the Python SDK's wording.
pub(crate) const ZERO_TIMEOUT: &str = "timeout must be a positive, finite number of seconds.";

/// Whether `c` is whitespace to Python's `str.strip()`, which is what the
/// Python SDK trims with: Unicode whitespace plus the four ASCII separators
/// U+001C to U+001F, which Python counts as whitespace and Rust's
/// `char::is_whitespace` does not.
fn is_python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Whether `text` is empty once Python's `str.strip()` has trimmed it.
fn is_blank(text: &str) -> bool {
    text.chars().all(is_python_space)
}

/// The variable `name` as `env` finds it, trimmed as [`is_python_space`]
/// says, or `None` when it is unset or blank.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error naming the
/// variable when its value is not UTF-8. The value itself is not repeated: it
/// may be the API key.
fn from_env<V>(env: &impl Fn(&str) -> Option<V>, name: &str) -> Result<Option<String>, Error>
where
    V: Into<OsString>,
{
    let Some(value) = env(name) else {
        return Ok(None);
    };
    let mut value = value.into().into_string().map_err(|_| {
        Error::config(format!("The {name} environment variable is not valid UTF-8."))
    })?;
    let end = value.trim_end_matches(is_python_space).len();
    value.truncate(end);
    let start = value.len() - value.trim_start_matches(is_python_space).len();
    value.drain(..start);
    Ok(if value.is_empty() { None } else { Some(value) })
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
