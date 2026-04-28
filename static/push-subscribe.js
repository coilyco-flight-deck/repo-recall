// PWA push subscription bootstrap.
//
// Registers the service worker, exposes a "enable notifications" button
// in the header that prompts for permission, subscribes to FCM via the
// browser's pushManager, and POSTs the resulting subscription to the
// server. Idempotent on repeat loads.
//
// See docs/push-notifications.md for the full flow.

(async function () {
  if (!("serviceWorker" in navigator) || !("PushManager" in window)) {
    return;
  }

  let registration;
  try {
    registration = await navigator.serviceWorker.register("/sw.js", { scope: "/" });
  } catch (e) {
    console.warn("repo-recall: SW register failed", e);
    return;
  }
  await navigator.serviceWorker.ready;

  const slot = document.getElementById("push-slot");
  if (!slot) return;

  function urlBase64ToUint8Array(b64) {
    const padding = "=".repeat((4 - (b64.length % 4)) % 4);
    const base64 = (b64 + padding).replace(/-/g, "+").replace(/_/g, "/");
    const raw = atob(base64);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; ++i) out[i] = raw.charCodeAt(i);
    return out;
  }

  async function ensureSubscribed() {
    const existing = await registration.pushManager.getSubscription();
    if (existing) {
      // Re-POST in case the server lost it (state DB wiped, redeploy on a
      // fresh box, etc.). Cheap; the upsert dedups by endpoint.
      await fetch("/api/push/subscribe", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(existing.toJSON()),
      });
      return existing;
    }
    const keyText = await fetch("/api/push/vapid-key").then((r) => r.text());
    const key = urlBase64ToUint8Array(keyText.trim());
    const sub = await registration.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: key,
    });
    await fetch("/api/push/subscribe", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(sub.toJSON()),
    });
    return sub;
  }

  function render() {
    slot.replaceChildren();
    const perm = Notification.permission;
    if (perm === "granted") {
      const span = document.createElement("span");
      span.textContent = "🔔 on";
      span.title = "push notifications enabled";
      span.className = "text-[#574f7d]/70 text-xs";
      slot.appendChild(span);
      ensureSubscribed().catch((e) =>
        console.warn("repo-recall: subscribe failed", e),
      );
      return;
    }
    if (perm === "denied") {
      // No programmatic way back. User has to clear the site permission
      // in browser settings to re-enable. Render nothing.
      return;
    }
    // perm === "default": user has not been asked yet.
    const btn = document.createElement("button");
    btn.type = "button";
    btn.textContent = "🔔 enable notifications";
    btn.className =
      "text-xs px-2 py-1 rounded-md border border-[#9e9fc2]/60 " +
      "bg-white/70 text-[#3e375d] hover:bg-white";
    btn.addEventListener("click", async () => {
      const result = await Notification.requestPermission();
      if (result === "granted") {
        try {
          await ensureSubscribed();
        } catch (e) {
          console.warn("repo-recall: subscribe failed", e);
        }
      }
      render();
    });
    slot.appendChild(btn);
  }

  render();
})();
