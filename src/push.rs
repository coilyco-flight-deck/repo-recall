// Web Push delivery for action-required signals.
//
// dispatch_for_signals is called by the refresh task once it knows the
// fresh action-required set. It diffs against the persisted seen-signals
// set, and for each genuinely-new <repo_id>:<signal> id sends one push
// per stored subscription. Subscriptions that come back 404 / 410 are
// pruned in place: the device unsubscribed in browser settings or
// uninstalled the PWA.
//
// See docs/push-notifications.md for the full architecture.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use web_push::{
    ContentEncoding, IsahcWebPushClient, SubscriptionInfo, VapidSignatureBuilder, WebPushClient,
    WebPushError, WebPushMessageBuilder,
};

use crate::state::{StateDb, Subscription};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub url: String,
    pub signal_id: String,
}

#[derive(Debug, Default)]
pub struct DispatchOutcome {
    pub new_signals: usize,
    pub pushes_sent: usize,
    pub pushes_failed: usize,
    pub gone_subscriptions: usize,
}

/// Sign + encrypt + POST one push to one subscription. Returns the raw
/// WebPushError so the caller can decide whether to prune the row
/// (404 / 410) or just log and move on.
async fn send_one(
    client: &IsahcWebPushClient,
    private_pem: &str,
    sub: &Subscription,
    payload: &PushPayload,
) -> std::result::Result<(), WebPushError> {
    let info = SubscriptionInfo::new(sub.endpoint.clone(), sub.p256dh.clone(), sub.auth.clone());
    let signature = VapidSignatureBuilder::from_pem(private_pem.as_bytes(), &info)?.build()?;

    let body = serde_json::to_vec(payload)
        .map_err(|e| WebPushError::Other(format!("payload serialize: {e}")))?;

    let mut builder = WebPushMessageBuilder::new(&info);
    builder.set_payload(ContentEncoding::Aes128Gcm, &body);
    builder.set_vapid_signature(signature);
    let message = builder.build()?;
    client.send(message).await
}

pub async fn dispatch_for_signals(
    state_db: &StateDb,
    new_signals: Vec<PushPayload>,
) -> DispatchOutcome {
    let mut out = DispatchOutcome {
        new_signals: new_signals.len(),
        ..Default::default()
    };
    if new_signals.is_empty() {
        return out;
    }

    let subs = match state_db.list_subscriptions() {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("list_subscriptions failed: {e:?}");
            return out;
        }
    };
    if subs.is_empty() {
        return out;
    }

    let vapid = match state_db.get_or_init_vapid() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("vapid init failed; skipping push dispatch: {e:?}");
            return out;
        }
    };
    let client = match IsahcWebPushClient::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("isahc client init failed; skipping push dispatch: {e:?}");
            return out;
        }
    };

    for payload in &new_signals {
        for sub in &subs {
            match send_one(&client, &vapid.private_pem, sub, payload).await {
                Ok(()) => out.pushes_sent += 1,
                Err(WebPushError::EndpointNotValid | WebPushError::EndpointNotFound) => {
                    if let Err(e) = state_db.remove_subscription(&sub.endpoint) {
                        tracing::debug!("remove_subscription({}) failed: {e:?}", sub.endpoint);
                    }
                    out.gone_subscriptions += 1;
                }
                Err(e) => {
                    tracing::debug!("push send to {} failed: {e:?}", sub.endpoint);
                    out.pushes_failed += 1;
                }
            }
        }
    }
    out
}

/// Build a PushPayload for one action-required signal. Title / body are
/// what the user sees on the lockscreen; url is where notificationclick
/// lands them. Kept here (not in routes/api.rs) so the dispatcher and
/// tests can build payloads without dragging in axum.
pub fn payload_for(repo_id: i64, repo_name: &str, signal: &str, detail: &str) -> PushPayload {
    let title = format!("{repo_name}: {}", humanize_signal(signal));
    PushPayload {
        title,
        body: detail.to_string(),
        url: format!("/repos/{repo_id}"),
        signal_id: format!("{repo_id}:{signal}"),
    }
}

fn humanize_signal(signal: &str) -> &'static str {
    match signal {
        "ci_failing" => "CI failing",
        "deploy_failing" => "deploy failing",
        "deploy_stale" => "deploy stale",
        "dirty_tree" => "dirty working tree",
        "in_progress_op" => "git op in progress",
        "detached_head" => "detached HEAD",
        "review_requested" => "review requested",
        "pr_no_reviewer" => "PR needs a reviewer",
        "issue_assigned" => "issue assigned",
        _ => "action required",
    }
}

/// Result of [`dispatch_for_signals`] coerced to an Ok type so callers
/// can use `?` without losing the count fields.
impl DispatchOutcome {
    pub fn ok(self) -> Result<Self> {
        Ok(self)
    }
}
