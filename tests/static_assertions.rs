//! Properties of the public types that are checked by the compiler: if one
//! of them stops holding, this file stops compiling.
//!
//! `Error` stays one pointer wide, the client can be shared across tasks, and
//! the future of every call can be sent to another thread - so a call can be
//! awaited inside `tokio::spawn`.

use std::{
    convert::Infallible,
    future::{Ready, ready},
    mem::size_of,
    task::{Context, Poll},
};

use http::{Request, Response};
use tower_service::Service;
use typesafe_sdk::{
    Body, Client, Error, HttpService, HyperResponseFuture, HyperTransport, Noul, Questions,
    ResponseBody, SystemOneResponse,
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

    let custom = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .build_with_service(Echo)
        .expect("the client builds");
    is_send(&custom.system_one(state.as_str(), &questions).send());
    is_send(&custom.system_one(&state, &questions).typed::<Ticket>().send());
    is_send(&custom.models().list().send());
    is_send(&custom.warm_up());

    // The typed call answers with the struct, read by field.
    let spam: fn(&SystemOneResponse<Ticket>) -> f64 = |response| response.answers().spam.noul();
    let _ = spam;
}
