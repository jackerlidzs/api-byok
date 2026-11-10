//! Answers Cursor `HealthService`, which only Cursor's Network Diagnostics call.
//!
//! With the local Cursor account, the official backend rejects these calls as unauthenticated,
//! so the API, Ping, Chat and Agent checks fail although BYOK models work. Calls made with the
//! local account are answered here; calls made with a real Cursor account still go upstream.
//!
//! `StreamBidi` reaches the server as `StreamBidiSSE` (the response stream) plus one BidiAppend
//! per client message; `HealthStreams` pairs the two by request id and echoes every message.
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{Extension, Request},
    http::{header, HeaderValue, Response, StatusCode},
};
use parking_lot::Mutex;
use prost::Message;
use tokio::sync::mpsc;

use crate::{
    api::cursor::{
        bidi,
        proxy::{self, CursorProxy},
    },
    cursor::protocol::{
        connect,
        proto::aiserver::v1::{BidiAppendRequest, BidiRequestId, HealthRequest, HealthResponse},
    },
    local_app, Error, Result,
};

const CONNECT_STREAM: &str = "application/connect+proto";
const SSE: &str = "text/event-stream";
/// Chunks per `Stream`/`StreamSSE` call; the diagnostics require at least five.
const STREAM_RESPONSES: usize = 5;
/// A `StreamBidiSSE` stream ends after this long without a client message.
const BIDI_IDLE: Duration = Duration::from_secs(30);
/// Messages appended for a stream that never opens are dropped after this long.
const UNOPENED_STREAM_TTL: Duration = Duration::from_secs(60);

/// Whether the call uses the local Cursor account; real accounts go to the official backend.
fn is_local(request: &Request<Body>) -> bool {
    local_app::request_uses_local_cursor_token(request.headers())
}

async fn read_body(request: Request<Body>) -> Result<Bytes> {
    to_bytes(request.into_body(), usize::MAX)
        .await
        .map_err(|error| Error::Protocol(format!("cannot read request body: {error}")))
}

pub async fn ping(
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    if !is_local(&request) {
        return proxy::forward(Extension(proxy), request).await;
    }
    let request: HealthRequest = connect::decode_unary(&read_body(request).await?)?;
    Ok(unary_response(&echo(request.payload)))
}

pub async fn unary(
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    if !is_local(&request) {
        return proxy::forward(Extension(proxy), request).await;
    }
    read_body(request).await?;
    Ok(unary_response(&echo("ok".into())))
}

/// `Stream`, used over HTTP/2.
pub async fn stream(
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    server_stream(proxy, request, CONNECT_STREAM).await
}

/// `StreamSSE`, used in HTTP/1.1 compatibility mode.
pub async fn stream_sse(
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    server_stream(proxy, request, SSE).await
}

async fn server_stream(
    proxy: CursorProxy,
    request: Request<Body>,
    content_type: &'static str,
) -> Result<Response<Body>> {
    if !is_local(&request) {
        return proxy::forward(Extension(proxy), request).await;
    }
    let request: HealthRequest = connect::decode_unary(&read_body(request).await?)?;
    let mut frames = Vec::with_capacity(STREAM_RESPONSES + 1);
    for _ in 0..STREAM_RESPONSES {
        frames.push(connect::encode_message(&echo(request.payload.clone()))?);
    }
    frames.push(connect::encode_end_stream());
    Ok(stream_response(Body::from(frames.concat()), content_type))
}

pub async fn stream_bidi_sse(
    Extension(proxy): Extension<CursorProxy>,
    Extension(streams): Extension<HealthStreams>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    if !is_local(&request) {
        return proxy::forward(Extension(proxy), request).await;
    }
    let request: BidiRequestId = connect::decode_unary(&read_body(request).await?)?;
    let receiver = streams.open(&request.request_id).ok_or_else(|| {
        Error::Protocol(format!(
            "health stream {} is already open",
            request.request_id
        ))
    })?;
    let open = OpenStream {
        streams,
        request_id: request.request_id,
    };
    Ok(stream_response(
        Body::from_stream(echo_frames(receiver, open)),
        SSE,
    ))
}

/// Echoes every queued client message until the stream idles, then ends the stream.
fn echo_frames(
    mut receiver: mpsc::UnboundedReceiver<String>,
    open: OpenStream,
) -> impl tokio_stream::Stream<Item = std::result::Result<Bytes, Infallible>> {
    async_stream::stream! {
        let _open = open;
        while let Ok(Some(payload)) = tokio::time::timeout(BIDI_IDLE, receiver.recv()).await {
            match connect::encode_message(&echo(payload)) {
                Ok(frame) => yield Ok::<Bytes, Infallible>(frame),
                Err(_) => break,
            }
        }
        yield Ok(connect::encode_end_stream());
    }
}

fn echo(payload: String) -> HealthResponse {
    HealthResponse { payload }
}

fn unary_response(message: &HealthResponse) -> Response<Body> {
    let mut response = Response::new(Body::from(message.encode_to_vec()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    response
}

/// Connect frames, with the headers RunSSE uses when they travel as `text/event-stream`.
fn stream_response(body: Body, content_type: &'static str) -> Response<Body> {
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if content_type == SSE {
        headers.insert("connect-protocol-version", HeaderValue::from_static("1"));
    }
    response
}

/// `StreamBidi` health streams by request id. A message's BidiAppend may arrive before or after
/// `StreamBidiSSE` opens the stream, so whichever comes first creates it.
#[derive(Clone, Default)]
pub struct HealthStreams {
    streams: Arc<Mutex<HashMap<String, PendingStream>>>,
}

struct PendingStream {
    sender: mpsc::UnboundedSender<String>,
    /// Taken by `StreamBidiSSE`; `None` once the stream is open.
    receiver: Option<mpsc::UnboundedReceiver<String>>,
    created: Instant,
}

impl HealthStreams {
    /// Queues a BidiAppend for a health stream that already exists. Checked before the Agent
    /// decoding, so a health message is never mistaken for an Agent message.
    pub fn append_to_existing(&self, request: &BidiAppendRequest) -> bool {
        let Some(request_id) = append_request_id(request) else {
            return false;
        };
        self.contains(request_id) && self.append(request_id, request)
    }

    /// Queues a BidiAppend that is not an Agent message as a health stream message. Covers the
    /// append that arrives before `StreamBidiSSE` opens its stream.
    pub fn append_new(&self, request: &BidiAppendRequest) -> bool {
        append_request_id(request).is_some_and(|request_id| self.append(request_id, request))
    }

    fn append(&self, request_id: &str, request: &BidiAppendRequest) -> bool {
        let Ok(payload) = bidi::payload(request) else {
            return false;
        };
        let Ok(message) = HealthRequest::decode(payload.as_ref()) else {
            return false;
        };
        self.push(request_id, message.payload);
        true
    }

    fn contains(&self, request_id: &str) -> bool {
        self.streams.lock().contains_key(request_id)
    }

    /// Queues one client message to be echoed on the stream.
    fn push(&self, request_id: &str, payload: String) {
        let mut streams = self.streams.lock();
        prune_unopened(&mut streams);
        let stream = streams
            .entry(request_id.to_owned())
            .or_insert_with(PendingStream::new);
        let _ = stream.sender.send(payload);
    }

    /// Opens the response side of a stream; `None` when it is already open.
    fn open(&self, request_id: &str) -> Option<mpsc::UnboundedReceiver<String>> {
        let mut streams = self.streams.lock();
        prune_unopened(&mut streams);
        streams
            .entry(request_id.to_owned())
            .or_insert_with(PendingStream::new)
            .receiver
            .take()
    }

    fn close(&self, request_id: &str) {
        self.streams.lock().remove(request_id);
    }
}

impl PendingStream {
    fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver: Some(receiver),
            created: Instant::now(),
        }
    }
}

fn prune_unopened(streams: &mut HashMap<String, PendingStream>) {
    streams.retain(|_, stream| {
        stream.receiver.is_none() || stream.created.elapsed() < UNOPENED_STREAM_TTL
    });
}

fn append_request_id(request: &BidiAppendRequest) -> Option<&str> {
    request
        .request_id
        .as_ref()
        .map(|id| id.request_id.as_str())
        .filter(|id| !id.is_empty())
}

/// Removes the stream when its response ends or the client disconnects.
struct OpenStream {
    streams: HealthStreams,
    request_id: String,
}

impl Drop for OpenStream {
    fn drop(&mut self) {
        self.streams.close(&self.request_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures_util::StreamExt;

    fn local_request(body: Vec<u8>) -> Request<Body> {
        let mut request = Request::new(Body::from(body));
        let authorization = local_app::local_cursor_authorization().parse().unwrap();
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, authorization);
        request
    }

    fn health_request(payload: &str) -> Vec<u8> {
        HealthRequest {
            payload: payload.into(),
        }
        .encode_to_vec()
    }

    /// Local-account calls never reach the official backend; the proxy only has to exist.
    async fn proxy() -> (tempfile::TempDir, CursorProxy) {
        let directory = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", directory.path().join("test.db").display());
        let store = crate::store::Store::connect(&url).await.unwrap();
        let clients = crate::network::NetworkClients::new(store);
        (directory, CursorProxy::cursor(clients))
    }

    async fn payloads(body: Body) -> (Vec<String>, bool) {
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        let frames = connect::decode_frames(&bytes).unwrap();
        let ended = frames.last().is_some_and(|(flags, payload)| {
            flags & connect::END_STREAM_FLAG != 0 && &payload[..] == b"{}"
        });
        let payloads = frames
            .iter()
            .filter(|(flags, _)| flags & connect::END_STREAM_FLAG == 0)
            .map(|(_, payload)| HealthResponse::decode(&payload[..]).unwrap().payload)
            .collect();
        (payloads, ended)
    }

    #[tokio::test]
    async fn ping_echoes_and_unary_answers_ok() {
        let (_directory, proxy) = proxy().await;
        let request = local_request(health_request("ping"));
        let response = ping(Extension(proxy.clone()), request).await.unwrap();
        let content_type = &response.headers()[header::CONTENT_TYPE];
        assert_eq!(content_type, "application/proto");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(HealthResponse::decode(body).unwrap().payload, "ping");

        let request = local_request(Vec::new());
        let response = unary(Extension(proxy), request).await.unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(HealthResponse::decode(body).unwrap().payload, "ok");
    }

    #[tokio::test]
    async fn server_streams_echo_five_chunks_then_end() {
        let (_directory, proxy) = proxy().await;
        let request = HealthRequest {
            payload: "foo".into(),
        };
        let framed = connect::encode_message(&request).unwrap().to_vec();
        let response = stream(Extension(proxy.clone()), local_request(framed))
            .await
            .unwrap();
        let content_type = &response.headers()[header::CONTENT_TYPE];
        assert_eq!(content_type, "application/connect+proto");
        let expected = (vec!["foo".to_owned(); STREAM_RESPONSES], true);
        assert_eq!(payloads(response.into_body()).await, expected);

        let request = local_request(health_request("foo"));
        let response = stream_sse(Extension(proxy), request).await.unwrap();
        let content_type = &response.headers()[header::CONTENT_TYPE];
        assert_eq!(content_type, "text/event-stream");
        let (chunks, ended) = payloads(response.into_body()).await;
        assert_eq!(chunks.len(), STREAM_RESPONSES);
        assert!(ended);
    }

    /// The BidiAppend Cursor sends for one `StreamBidi` message, binary or hex encoded.
    fn health_append(request_id: &str, payload: &str, hex_encoded: bool) -> BidiAppendRequest {
        let message = health_request(payload);
        BidiAppendRequest {
            data: if hex_encoded {
                hex::encode(&message)
            } else {
                String::new()
            },
            request_id: Some(BidiRequestId {
                request_id: request_id.into(),
            }),
            append_seqno: 0,
            data_binary: if hex_encoded { Vec::new() } else { message },
        }
    }

    #[test]
    fn diagnostics_append_is_not_an_agent_message() {
        // Network Diagnostics push `{payload: "foo"}`; decoding that as an AgentClientMessage
        // fails with "invalid wire type value: 6", the error Cursor showed for the Agent check.
        let append = health_append("req-0", "foo", false);
        let Err(error) = bidi::decode(&append) else {
            panic!("a health message must not decode as an Agent message");
        };
        assert!(error.to_string().contains("invalid wire type value: 6"));
        let streams = HealthStreams::default();
        assert!(!streams.append_to_existing(&append), "no stream exists yet");
        assert!(streams.append_new(&append));
        assert!(streams.append_to_existing(&health_append("req-0", "foo", true)));
    }

    #[tokio::test]
    async fn bidi_stream_echoes_messages_appended_before_and_after_it_opens() {
        let (_directory, proxy) = proxy().await;
        let streams = HealthStreams::default();
        assert!(streams.append_new(&health_append("req-1", "early", false)));
        let opened = BidiRequestId {
            request_id: "req-1".into(),
        }
        .encode_to_vec();
        let response = stream_bidi_sse(
            Extension(proxy.clone()),
            Extension(streams.clone()),
            local_request(opened.clone()),
        )
        .await
        .unwrap();
        assert_eq!(response.headers()["connect-protocol-version"], "1");
        let reopened = stream_bidi_sse(
            Extension(proxy),
            Extension(streams.clone()),
            local_request(opened),
        )
        .await;
        assert!(reopened.is_err(), "a stream opens once");
        assert!(streams.append_to_existing(&health_append("req-1", "late", true)));

        let mut body = response.into_body().into_data_stream();
        for expected in ["early", "late"] {
            let frame = body.next().await.unwrap().unwrap();
            let (_, payload) = connect::decode_frames(&frame).unwrap().remove(0);
            assert_eq!(HealthResponse::decode(payload).unwrap().payload, expected);
        }
        drop(body);
        let closed = !streams.contains("req-1");
        assert!(closed, "closing the response removes the stream");
    }
}
