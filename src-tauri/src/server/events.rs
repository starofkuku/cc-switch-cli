//! Write-event broadcast bus and the SSE endpoint.
//!
//! Scope (v1): only writes handled by this server process emit events — a
//! concurrent CLI invocation is not visible here. Clients should poll as a
//! fallback; widening coverage would require hooks inside the services layer
//! (upstream-synced code) and is deferred.

use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::ServerState;

#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Arc<String>>,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        let (sender, _) = broadcast::channel(64);
        Self { sender }
    }

    pub(crate) fn emit(&self, kind: &str, app: Option<&str>, detail: Value) {
        let payload = json!({
            "type": kind,
            "app": app,
            "detail": detail,
            "at": chrono::Utc::now().to_rfc3339(),
        })
        .to_string();
        // A full buffer only means there are no live SSE subscribers.
        let _ = self.sender.send(Arc::new(payload));
    }
}

pub(crate) async fn events_route(
    State(state): State<ServerState>,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    let mut receiver = state.events.sender.subscribe();
    let stream = async_stream::stream! {
        yield Ok(Event::default().event("hello").data("{\"type\":\"hello\"}"));
        loop {
            match receiver.recv().await {
                Ok(payload) => yield Ok(Event::default().event("change").data(payload.as_str())),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    yield Ok(Event::default().event("lagged").data(format!("{{\"skipped\":{n}}}")));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}
