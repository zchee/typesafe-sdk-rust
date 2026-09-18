//! S4: the HTTP/2 SETTINGS `api.typesafe.ai` sends, read without a credential.
//!
//! Two readings of the same connection. The first opens a TLS stream, writes
//! the HTTP/2 connection preface by hand and decodes the server's first
//! SETTINGS frame off the wire, which gives every parameter rather than the
//! two hyper surfaces. The second hands an identical stream to the `h2` crate
//! and asks it for `max_concurrent_send_streams()` and for the send capacity
//! it grants on a fresh stream before any WINDOW_UPDATE.
//!
//! No API key is read or sent. The POST used to open a stream carries no body
//! bytes and is reset immediately; an unauthenticated 403 is the expected
//! answer.

use std::{error::Error, sync::Arc, time::Duration};

use http::Request;
use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

use crate::tls;

const HOST: &str = "api.typesafe.ai";
const PORT: u16 = 443;
/// RFC 9113 section 3.4.
const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub async fn run() -> Result<(), Box<dyn Error>> {
    println!("# S4 server SETTINGS of {HOST}, no credential");
    println!();
    println!("## raw frame read: every parameter the server announces");
    raw_settings().await?;
    println!();
    println!("## through the h2 crate: the two values a client acts on");
    h2_view().await?;
    println!();
    println!("Keep-alive behaviour is NOT measured here: it needs an authenticated idle test.");
    Ok(())
}

/// Opens a TLS connection with ALPN h2 and returns the negotiated stream.
async fn connect() -> Result<tokio_rustls::client::TlsStream<TcpStream>, Box<dyn Error>> {
    let mut config = tls::client_config(Vec::new())?;
    // This configuration is NOT handed to hyper-rustls, so setting ALPN here is
    // correct: the h2 path has to ask for `h2` itself.
    config.alpn_protocols = vec![b"h2".to_vec()];

    let stream =
        tokio::time::timeout(Duration::from_secs(20), TcpStream::connect((HOST, PORT))).await??;
    stream.set_nodelay(true)?;

    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(HOST)?.to_owned();
    let stream = connector.connect(server_name, stream).await?;

    let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
    println!("alpn={:?}", alpn.as_deref().map(String::from_utf8_lossy));
    Ok(stream)
}

/// Writes the preface and an empty SETTINGS frame, then decodes the server's.
async fn raw_settings() -> Result<(), Box<dyn Error>> {
    let mut stream = connect().await?;

    stream.write_all(PREFACE).await?;
    // An empty SETTINGS frame: length 0, type 0x4, no flags, stream 0.
    stream.write_all(&[0, 0, 0, 0x04, 0x00, 0, 0, 0, 0]).await?;
    stream.flush().await?;

    let deadline = Duration::from_secs(20);
    let mut header = [0u8; 9];
    for _ in 0..8 {
        tokio::time::timeout(deadline, stream.read_exact(&mut header)).await??;
        let length =
            usize::from(header[0]) << 16 | usize::from(header[1]) << 8 | usize::from(header[2]);
        let kind = header[3];
        let flags = header[4];
        let mut payload = vec![0u8; length];
        tokio::time::timeout(deadline, stream.read_exact(&mut payload)).await??;

        match kind {
            0x04 if flags & 0x01 == 0 => {
                println!("SETTINGS frame, {length} bytes, {} parameters:", length / 6);
                // Each parameter is a fixed 6-byte record, so `as_chunks`
                // hands them over as arrays and the indices below are checked
                // once by the compiler rather than on every access.
                let (parameters, _remainder) = payload.as_chunks::<6>();
                for &[id_hi, id_lo, a, b, c, d] in parameters {
                    let id = u16::from_be_bytes([id_hi, id_lo]);
                    let value = u32::from_be_bytes([a, b, c, d]);
                    println!("  {:<32} = {value}", setting_name(id));
                }
                // A server may follow SETTINGS with a connection-level
                // WINDOW_UPDATE, which changes the effective send window, so
                // the loop keeps reading rather than stopping here.
            }
            0x04 => println!("SETTINGS ack"),
            0x08 => {
                let increment =
                    u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]])
                        & 0x7fff_ffff;
                let stream_id =
                    u32::from_be_bytes([header[5], header[6], header[7], header[8]]) & 0x7fff_ffff;
                println!("WINDOW_UPDATE on stream {stream_id}: +{increment}");
                break;
            }
            other => {
                println!("frame type {other:#04x}, {length} bytes, flags {flags:#04x}");
                break;
            }
        }
    }

    Ok(())
}

fn setting_name(id: u16) -> &'static str {
    match id {
        0x1 => "SETTINGS_HEADER_TABLE_SIZE",
        0x2 => "SETTINGS_ENABLE_PUSH",
        0x3 => "SETTINGS_MAX_CONCURRENT_STREAMS",
        0x4 => "SETTINGS_INITIAL_WINDOW_SIZE",
        0x5 => "SETTINGS_MAX_FRAME_SIZE",
        0x6 => "SETTINGS_MAX_HEADER_LIST_SIZE",
        0x8 => "SETTINGS_ENABLE_CONNECT_PROTOCOL",
        0x9 => "SETTINGS_NO_RFC7540_PRIORITIES",
        _ => "unknown setting",
    }
}

/// The same connection through `h2`, which is what hyper's HTTP/2 client uses.
async fn h2_view() -> Result<(), Box<dyn Error>> {
    let stream = connect().await?;
    let (send_request, mut connection) = h2::client::handshake(stream).await?;

    // The connection future has to be polled for the server's SETTINGS to be
    // read and acknowledged. Polling it under a timeout drives it without
    // giving it away, so `max_concurrent_send_streams()` can still be asked.
    let settled = tokio::time::timeout(Duration::from_millis(500), &mut connection).await;
    if let Ok(outcome) = settled {
        println!("connection ended during the settle window: {outcome:?}");
        return Ok(());
    }
    println!("max_concurrent_send_streams = {}", connection.max_concurrent_send_streams());

    let probe = async {
        let mut send_request = send_request.ready().await?;
        // A POST with no body bytes: enough to own a stream and ask for send
        // capacity. It is reset before any payload is written.
        let request = Request::post(format!("https://{HOST}/v1/systemone"))
            .header("content-type", "application/json")
            .header("user-agent", "typesafe-sdk-rust-spike/0.0.0")
            .body(())?;
        let (response, mut body) = send_request.send_request(request, false)?;

        body.reserve_capacity(16 * 1024 * 1024);
        let granted = std::future::poll_fn(|cx| body.poll_capacity(cx)).await;
        println!("first poll_capacity grant = {granted:?}");
        println!("stream capacity after the grant = {}", body.capacity());

        body.send_reset(h2::Reason::CANCEL);
        // The response head is still worth reporting when the server answers
        // before the reset lands: 403 is the expected unauthenticated answer.
        match tokio::time::timeout(Duration::from_secs(5), response).await {
            Ok(Ok(response)) => println!("response status = {}", response.status().as_u16()),
            Ok(Err(error)) => println!("response error (expected after the reset) = {error}"),
            Err(_) => println!("no response head within 5 s"),
        }
        Ok::<(), Box<dyn Error>>(())
    };

    tokio::select! {
        outcome = &mut connection => println!("connection ended: {outcome:?}"),
        outcome = probe => outcome?,
    }
    Ok(())
}
