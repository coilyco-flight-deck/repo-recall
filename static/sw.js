// repo-recall service worker.
//
// Three jobs:
//   1. Precache same-origin static assets (CSS, JS, icons, manifest)
//      so the PWA shell installs cleanly and survives flaky tailnet.
//   2. Handle `push` events from FCM: show an OS notification.
//   3. Handle `notificationclick`: focus the existing PWA window
//      or open it at the URL the server embedded in the payload.
//
// We deliberately do NOT cache /api/*, /repos/*, /sessions/*, /search,
// or the dashboard HTML at /. A stale dashboard is worse than an
// honest connection error.
//
// See docs/push-notifications.md for the full architecture.

const CACHE_VERSION = "repo-recall-shell-v1";

const SHELL_ASSETS = [
  "/static/style.css",
  "/static/icons/icon-192.png",
  "/static/icons/icon-512.png",
  "/static/manifest.webmanifest",
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE_VERSION).then((cache) => cache.addAll(SHELL_ASSETS))
  );
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(
        keys
          .filter((k) => k !== CACHE_VERSION)
          .map((k) => caches.delete(k))
      )
    )
  );
  self.clients.claim();
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);

  // Same-origin /static/* only. Everything else falls through
  // to the network so the browser handles it as if the SW was not here.
  if (url.origin !== self.location.origin) return;
  if (!url.pathname.startsWith("/static/")) return;

  event.respondWith(
    caches.match(event.request).then((cached) => {
      if (cached) return cached;
      return fetch(event.request).then((response) => {
        // Best-effort populate the cache for assets we did not list
        // explicitly (e.g. JS files not in SHELL_ASSETS).
        if (response.ok) {
          const copy = response.clone();
          caches.open(CACHE_VERSION).then((cache) => cache.put(event.request, copy));
        }
        return response;
      });
    })
  );
});

self.addEventListener("push", (event) => {
  // The server sends JSON: { title, body, url, signal_id }
  // Fall back to a generic toast if the payload is missing or malformed,
  // because Chrome revokes subscriptions when userVisibleOnly:true is
  // set and a push event does not result in a visible notification.
  let payload = { title: "repo-recall", body: "Action required", url: "/" };
  try {
    if (event.data) payload = { ...payload, ...event.data.json() };
  } catch (_) {
    // payload stays as fallback
  }

  const options = {
    body: payload.body,
    icon: "/static/icons/icon-192.png",
    badge: "/static/icons/icon-192.png",
    tag: payload.signal_id || undefined,
    renotify: !!payload.signal_id,
    data: { url: payload.url || "/" },
  };

  event.waitUntil(self.registration.showNotification(payload.title, options));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const target = (event.notification.data && event.notification.data.url) || "/";
  event.waitUntil(
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((windows) => {
        for (const w of windows) {
          if ("focus" in w) {
            w.navigate(target);
            return w.focus();
          }
        }
        if (self.clients.openWindow) return self.clients.openWindow(target);
      })
  );
});
