//! Sending a request and reading its server-sent events: the same for every
//! protocol, apart from how each `data:` payload is read.

use super::sse::SseParser;
use super::{http_error, Capture, EventStream, StreamEvent};
use futures::StreamExt;
use serde_json::Value;
use solos_api::CoreError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// What a protocol makes of one `data:` payload: events, or the end.
pub trait ChunkReader: Send + 'static {
    /// `None` ends the stream (`[DONE]`, `message_stop`).
    fn read(&mut self, data: &str) -> Option<Result<Vec<StreamEvent>, CoreError>>;
}

/// Post `body`, check the status, and stream what `reader` makes of the
/// events. The request body and the raw stream go to `capture` when it is on.
pub async fn post_sse(
    request: reqwest::RequestBuilder,
    body: &Value,
    capture: &Capture,
    cancel: CancellationToken,
    mut reader: impl ChunkReader,
) -> Result<EventStream, CoreError> {
    let stamp = crate::store::now_millis();
    capture.write(&format!("{stamp}-request.json"), &serde_json::to_vec_pretty(body).unwrap_or_default());
    let send = request.header("Accept", "text/event-stream").json(body).send();
    let resp = tokio::select! {
        r = send => r.map_err(|e| CoreError::Network { detail: e.to_string() })?,
        _ = cancel.cancelled() => return Err(CoreError::Internal { detail: "cancelled".into() }),
    };
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        capture.write(&format!("{stamp}-error.txt"), text.as_bytes());
        return Err(http_error(status, &text));
    }

    let (tx, mut rx) = mpsc::channel::<Result<StreamEvent, CoreError>>(256);
    let capture = capture.clone();
    let stream_name = format!("{stamp}-stream.sse");
    tokio::spawn(async move {
        let mut bytes = resp.bytes_stream();
        let mut sse = SseParser::default();
        // Sends what one payload produced; false when the stream is over.
        async fn deliver(
            tx: &mpsc::Sender<Result<StreamEvent, CoreError>>,
            read: Option<Result<Vec<StreamEvent>, CoreError>>,
        ) -> bool {
            match read {
                None => false,
                Some(Err(e)) => {
                    let _ = tx.send(Err(e)).await;
                    false
                }
                Some(Ok(evs)) => {
                    for ev in evs {
                        if tx.send(Ok(ev)).await.is_err() {
                            return false;
                        }
                    }
                    true
                }
            }
        }
        'read: loop {
            let next = tokio::select! {
                n = bytes.next() => n,
                _ = cancel.cancelled() => break,
            };
            match next {
                Some(Ok(chunk)) => {
                    capture.append(&stream_name, &chunk);
                    for data in sse.push(&chunk) {
                        if !deliver(&tx, reader.read(&data)).await {
                            break 'read;
                        }
                    }
                }
                Some(Err(e)) => {
                    let _ = tx.send(Err(CoreError::Network { detail: e.to_string() })).await;
                    break;
                }
                None => {
                    if let Some(data) = sse.finish() {
                        deliver(&tx, reader.read(&data)).await;
                    }
                    break;
                }
            }
        }
    });
    Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed())
}

pub fn client() -> reqwest::Client {
    super::install_crypto_provider();
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("an HTTP client with default settings")
}

/// A GET that must answer JSON, for model lists.
pub async fn get_json(request: reqwest::RequestBuilder) -> Result<Value, CoreError> {
    let resp = request.send().await.map_err(|e| CoreError::Network { detail: e.to_string() })?;
    let status = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| CoreError::Network { detail: e.to_string() })?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &body));
    }
    serde_json::from_str(&body).map_err(|e| CoreError::Protocol { detail: e.to_string() })
}
