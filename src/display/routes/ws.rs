use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};

/// `/livereload` — held open by the browser while the dev server runs.
/// `cargo-watch` restarts drop the socket, the client reconnects and reloads
/// the page. Production users hit this endpoint too; it's just always
/// connected and never triggers.
pub async fn livereload_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|socket| async move {
        let (mut sender, mut receiver) = socket.split();
        let _ = sender.send(Message::Text("ready".into())).await;
        while let Some(Ok(msg)) = receiver.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
}
