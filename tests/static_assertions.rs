//! Properties of the public types that are checked by the compiler: if one
//! of them stops holding, this file stops compiling.
//!
//! `Error` stays one pointer wide, the client and a retry policy can be shared
//! across tasks, and the future of every call - with a retry policy of its
//! own or the client's - can be sent to another thread, so a call can be
//! awaited inside `tokio::spawn`.

use std::{
    convert::Infallible,
    future::{Ready, ready},
    mem::{size_of, size_of_val},
    task::{Context, Poll},
};

use http::{Request, Response};
use tower_service::Service;
use typesafe_sdk::{
    Body, Client, Error, HttpService, HyperResponseFuture, HyperTransport, Noul, Questions,
    ResponseBody, RetryPolicy, StatusSet, SystemOneResponse,
    de::{AnswerContext, AnswerSet},
    response::NoulAnswer,
};

// `Result<T, Error>` costs a pointer beside `T`.
const _: () = assert!(size_of::<Error>() == size_of::<usize>());

const fn error_is_shareable<T: Send + Sync + 'static>() {}
const _: () = error_is_shareable::<Error>();

const fn client_is_shareable<T: Clone + Send + Sync>() {}
const _: () = client_is_shareable::<Client>();
const _: () = client_is_shareable::<Client<HyperTransport>>();
const _: () = client_is_shareable::<Client<Echo>>();

// A policy, predicate included, can be built once and handed to every task.
const fn policy_is_shareable<T: Clone + Send + Sync + 'static>() {}
const _: () = policy_is_shareable::<RetryPolicy>();
const fn is_copy<T: Copy + Send + Sync + 'static>() {}
const _: () = is_copy::<StatusSet>();

// The default transport answers with this crate's own body type, not hyper's,
// and both it and the future that yields it can move to another thread.
const fn crosses_threads<T: Send + 'static>() {}
const _: () = crosses_threads::<ResponseBody>();
const _: () = crosses_threads::<HyperResponseFuture>();
const _: fn(<HyperTransport as HttpService>::ResponseBody) -> ResponseBody = |body| body;

/// A transport of the caller's own, for the bounds over a custom `S`.
#[derive(Debug, Clone, Copy)]
struct Echo;

impl Service<Request<Body>> for Echo {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Ready<Result<Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        ready(Ok(Response::new(request.into_body())))
    }
}

/// A struct answer set, standing in for what `ask::<T>()` will decode into:
/// that call is `system_one(state, T::prepared()).typed::<T>()`.
struct Ticket {
    spam: NoulAnswer,
}

impl AnswerSet for Ticket {
    fn deserialize_answers<'de, D>(deserializer: D, _: AnswerContext) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Fields {
            spam: NoulAnswer,
        }
        let Fields { spam } = serde::Deserialize::deserialize(deserializer)?;
        Ok(Self { spam })
    }
}

fn is_send<T: Send>(_: &T) {}

/// Every call's future is `Send`, over the default transport and a custom
/// one; the futures are built and dropped, never polled.
#[test]
fn the_future_of_every_call_is_send() {
    let questions =
        Questions::new().noul("spam", Noul::new().instructions("?")).prepare().expect("prepares");
    let state = String::from("hello");

    let client = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .build()
        .expect("the client builds");
    is_send(&client.system_one(state.as_str(), &questions).send());
    is_send(&client.system_one(&state, &questions).typed::<Ticket>().send());
    is_send(&client.models().list().send());
    is_send(&client.warm_up());
    let policy = RetryPolicy::default().predicate(|_| true);
    is_send(&client.system_one(state.as_str(), &questions).retry(policy.clone()).send());
    is_send(&client.models().list().retry(policy.clone()).send());

    let custom = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .build_with_service(Echo)
        .expect("the client builds");
    is_send(&custom.system_one(state.as_str(), &questions).send());
    is_send(&custom.system_one(&state, &questions).typed::<Ticket>().send());
    is_send(&custom.models().list().send());
    is_send(&custom.warm_up());
    is_send(&custom.system_one(&state, &questions).typed::<Ticket>().retry(policy.clone()).send());
    is_send(&custom.models().list().retry(policy).send());

    // The typed call answers with the struct, read by field.
    let spam: fn(&SystemOneResponse<Ticket>) -> f64 = |response| response.answers().spam.noul();
    let _ = spam;
}

/// A guard on the size of every call's future, unpolled, over the default
/// transport and a custom one. Tokio boxes a future larger than its
/// `BOX_FUTURE_THRESHOLD` (16384 bytes in a release build, 2048 in a debug
/// one) when it is spawned or blocked on, so a change that grows a call's
/// future is meant to be seen here, not found in a profile. The bounds sit
/// just above the sizes measured when the retry loop landed; raising one is a
/// decision to state, not a number to bump.
#[test]
fn the_future_of_every_call_stays_small() {
    // Measured over the default transport, whose response future is the
    // larger one: 2760, 2744 and 2448 bytes (2392, 2376 and 2080 over a
    // transport with a small future), in both profiles.
    const SYSTEM_ONE: usize = 2816;
    const TYPED: usize = 2816;
    const MODELS: usize = 2560;

    let questions =
        Questions::new().noul("spam", Noul::new().instructions("?")).prepare().expect("prepares");
    let state = String::from("hello");
    let client = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .build()
        .expect("the client builds");
    let custom = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .build_with_service(Echo)
        .expect("the client builds");

    let sizes = [
        (
            "system_one",
            size_of_val(&client.system_one(state.as_str(), &questions).send()),
            SYSTEM_ONE,
        ),
        (
            "typed",
            size_of_val(&client.system_one(&state, &questions).typed::<Ticket>().send()),
            TYPED,
        ),
        ("models", size_of_val(&client.models().list().send()), MODELS),
        (
            "system_one, custom",
            size_of_val(&custom.system_one(state.as_str(), &questions).send()),
            SYSTEM_ONE,
        ),
        (
            "typed, custom",
            size_of_val(&custom.system_one(&state, &questions).typed::<Ticket>().send()),
            TYPED,
        ),
        ("models, custom", size_of_val(&custom.models().list().send()), MODELS),
    ];
    for (call, size, bound) in sizes {
        println!("{call:<20} {size:>6} bytes (bound {bound})");
    }
    for (call, size, bound) in sizes {
        assert!(size <= bound, "the {call} future is {size} bytes, over its bound of {bound}");
    }
}
