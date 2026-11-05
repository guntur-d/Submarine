//! Backend → UI event sink shared by both runtimes.
//!
//! Desktop commands own an `AppHandle` and emit straight into the Tauri
//! event bridge. `submarine serve` has no WebView — it fans the same events
//! out over WebSockets. `Emit` is the seam: command bodies call
//! `app.emit(event, payload)` and never know which runtime they run in.
//!
//! Shapes match deliberately: `emit(&str, impl Serialize + Clone)` compiles
//! against both variants, so porting a command is a signature-only change.

use serde::Serialize;
use tokio::sync::broadcast;

/// A backend → UI event: the Tauri event name plus its JSON payload.
pub type BusEvent = (String, serde_json::Value);

/// Cloneable, `'static`, `Send + Sync` — safe to move into spawned workers,
/// exactly like the `AppHandle` it replaces at call sites.
#[derive(Clone)]
pub struct Emit {
    inner: EmitInner,
}

#[derive(Clone)]
enum EmitInner {
    Tauri(tauri::AppHandle),
    Serve(broadcast::Sender<BusEvent>),
}

impl From<&tauri::AppHandle> for Emit {
    fn from(app: &tauri::AppHandle) -> Self {
        Self { inner: EmitInner::Tauri(app.clone()) }
    }
}

impl From<tauri::AppHandle> for Emit {
    fn from(app: tauri::AppHandle) -> Self {
        Self { inner: EmitInner::Tauri(app) }
    }
}

impl Emit {
    /// `serve`-side constructor. The sender is the fan-out hub owned by the
    /// gateway; every `Emit::serve` clone publishes into the same bus.
    pub fn serve(tx: &broadcast::Sender<BusEvent>) -> Self {
        Self { inner: EmitInner::Serve(tx.clone()) }
    }

    /// Fire-and-forget, mirroring `let _ = app.emit(...)`: a dead frontend
    /// (closed window, lagged WS receiver) must never fail a backend op.
    pub fn emit(&self, event: &str, payload: impl Serialize + Clone) {
        match &self.inner {
            EmitInner::Tauri(app) => {
                use tauri::Emitter;
                let _ = app.emit(event, payload);
            }
            EmitInner::Serve(tx) => {
                // `send` fails only when no receiver is alive — same
                // no-listener case Tauri swallows. Lagged receivers are
                // skipped by the fan-out task, never here.
                let _ = tx.send((event.to_string(), serde_json::json!(payload)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serve_emit_reaches_broadcast_receiver() {
        let (tx, mut rx) = broadcast::channel::<BusEvent>(16);
        let emit = Emit::serve(&tx);
        emit.emit("session-log-test", serde_json::json!({"msg": "hi"}));
        let (event, payload) = rx.recv().await.expect("event delivered");
        assert_eq!(event, "session-log-test");
        assert_eq!(payload, serde_json::json!({"msg": "hi"}));
    }

    #[tokio::test]
    async fn serve_emit_without_receivers_does_not_panic() {
        let (tx, _) = broadcast::channel::<BusEvent>(16);
        drop(tx.subscribe());
        Emit::serve(&tx).emit("no-listener", serde_json::json!({}));
    }
}
