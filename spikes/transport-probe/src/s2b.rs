//! S2b: how many TCP connections 64 concurrent requests open on a cold client.
//!
//! hyper-util's legacy pool takes its single-connection lock only for
//! `Ver::Http2`, so a client that has not yet negotiated ALPN may open one
//! socket per in-flight request. The TestServer's base URL is an IP literal,
//! so no second socket is raced across resolved addresses and the count is the
//! pool's behaviour alone.

use std::{error::Error, time::Duration};

use http::Request;
use http_body_util::{BodyExt as _, Empty, Full};
use hyper::body::Bytes;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioTimer},
};
use rustls::pki_types::CertificateDer;
use test_support::{Protocol, TestServer};

use crate::{
    counting::{Counters, CountingConnector},
    tls,
};

const CONCURRENCY: usize = 64;
const REPETITIONS: usize = 10;

/// Which ALPN and pool configuration the client uses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `enable_http1().enable_http2()`, pool defaults: ALPN offers both.
    Auto,
    /// `http2_only(true)` with ALPN offering h2 only.
    Http2Only,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "A: Auto (h1+h2 ALPN, pool default)",
            Self::Http2Only => "B: http2_only(true), ALPN h2 only",
        }
    }
}

type HttpsClient =
    Client<CountingConnector<hyper_rustls::HttpsConnector<HttpConnector>>, Empty<Bytes>>;

pub async fn run() -> Result<(), Box<dyn Error>> {
    println!("# S2b cold fan-out: {CONCURRENCY} concurrent requests, {REPETITIONS} repetitions,");
    println!("# each repetition on a brand-new client, against the HTTP/2 + TLS TestServer");

    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        http::Response::new(Full::new(Bytes::from_static(b"{\"models\":[]}")))
    })
    .await?;
    let certificate = server.certificate_der().ok_or("the TLS server has a certificate")?;

    println!();
    println!(
        "{:<36} {:>6} {:>7} {:>7} {:>7} {:>14} {:>12}",
        "case", "warm", "min", "median", "max", "client_opened", "live_after_1s"
    );
    println!("# min/median/max are connections the SERVER accepted for the 64-way fan-out;");
    println!("# client_opened is the client's own total for the whole repetition.");

    for mode in [Mode::Auto, Mode::Http2Only] {
        for warm in [false, true] {
            let mut opened = Vec::with_capacity(REPETITIONS);
            let mut open_later = Vec::with_capacity(REPETITIONS);
            let mut client_opened = Vec::with_capacity(REPETITIONS);

            for _ in 0..REPETITIONS {
                let (client, counters) = build_client(mode, certificate.clone())?;
                let url = server.url("/v1/models");

                if warm {
                    fan_out(&client, &url, 1).await;
                }
                let before = server.accepted_connections();
                fan_out(&client, &url, CONCURRENCY).await;
                let accepted = server.accepted_connections() - before;
                opened.push(usize::try_from(accepted).unwrap_or(usize::MAX));

                // The client is still alive here, so the live counter is the
                // pool's idle set rather than a leak.
                tokio::time::sleep(Duration::from_secs(1)).await;
                open_later.push(counters.live());
                client_opened.push(counters.opened());
                drop(client);
            }

            opened.sort_unstable();
            println!(
                "{:<36} {:>6} {:>7} {:>7} {:>7} {:>14} {:>12}",
                mode.label(),
                if warm { "yes" } else { "no" },
                opened.first().copied().unwrap_or_default(),
                opened[opened.len() / 2],
                opened.last().copied().unwrap_or_default(),
                median(&mut client_opened),
                median(&mut open_later),
            );
        }
    }

    println!();
    println!("requests served in total = {}", server.request_count());
    Ok(())
}

fn build_client(
    mode: Mode,
    certificate: CertificateDer<'static>,
) -> Result<(HttpsClient, Counters), Box<dyn Error>> {
    let config = tls::client_config(vec![certificate])?;
    let builder = hyper_rustls::HttpsConnectorBuilder::new().with_tls_config(config).https_only();
    let connector = match mode {
        Mode::Auto => builder.enable_http1().enable_http2().build(),
        Mode::Http2Only => builder.enable_http2().build(),
    };

    let (connector, counters) = CountingConnector::new(connector);

    let mut client = Client::builder(TokioExecutor::new());
    // hyper panics on time-based pool options without a timer.
    client.timer(TokioTimer::new()).pool_idle_timeout(Duration::from_secs(90));
    if mode == Mode::Http2Only {
        client.http2_only(true);
    }
    Ok((client.build(connector), counters))
}

/// Issues `count` requests at once and drains every response body.
async fn fan_out(client: &HttpsClient, url: &str, count: usize) {
    let mut tasks = Vec::with_capacity(count);
    for _ in 0..count {
        let client = client.clone();
        let url = url.to_owned();
        tasks.push(tokio::spawn(async move {
            let request = Request::get(url).body(Empty::new())?;
            let response = client.request(request).await?;
            // The body must be consumed, or the stream stays open and the pool
            // cannot reuse the connection.
            let _ = response.into_body().collect().await?;
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }));
    }
    for task in tasks {
        if let Ok(Err(error)) = task.await {
            println!("  request failed: {error}");
        }
    }
}

fn median(values: &mut [usize]) -> usize {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or_default()
}
