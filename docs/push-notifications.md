# Push notifications

How the PWA delivers OS-level notifications for action-required signals on Android, even when the app is closed. Tracking issue: [#15](https://github.com/coilysiren/repo-recall/issues/15).

## What this gives you

Install repo-recall on your Android home screen, allow notifications once, and the OS will toast you when CI breaks, a working tree goes dirty, a rebase gets stuck, or a HEAD detaches on any indexed repo. The phone does not need a tab open. The PWA does not need to be running.

## Architecture in one paragraph

The browser, not repo-recall, owns the push subscription. Chrome on Android registers with FCM (Firebase Cloud Messaging) and hands the page a `PushSubscription` object. The page POSTs that subscription to repo-recall, which stores it in a persistent state file. When repo-recall sees a newly-appeared action-required signal between scans, it POSTs an encrypted payload to the FCM endpoint URL inside the subscription, signed with its VAPID private key. FCM relays to the device. The device wakes the service worker. The service worker calls `showNotification`. Tap the notification, the PWA opens at the affected repo.

## The pieces

### VAPID keypair

Voluntary Application Server Identification. A single ECDSA P-256 keypair, generated once on first run, stored in the persistent state file. The public half ships to the browser (so the browser can ask FCM to only accept pushes signed by us). The private half stays on kai-server and signs every push we send.

If we lose the keypair, every existing subscription becomes useless. New subscriptions work fine. There is no recovery path other than "everyone re-subscribes." We do not rotate.

### Persistent state file

`~/.local/share/repo-recall/state.redb`. Separate from the cache DB at `$TMPDIR/repo-recall-<port>/cache.redb` because the cache DB is wiped and rebuilt on every process start. State that must outlive restarts goes here:

- `vapid` table, single row, holds the keypair PEMs.
- `subscriptions` table, one row per device, holds `{endpoint, p256dh, auth, created_at, label}`.
- `seen_signals` table, holds the set of `<repo_id>:<signal>` ids we have already notified for. Survives restarts so we do not re-send a notification for every currently-broken repo every time the server boots.

The cache DB stays wipe-on-restart, the state DB stays persistent, and they never reference each other by foreign key. Different lifetimes, different files.

### Service worker

`static/sw.js`. Registered by the page on load. Three responsibilities:

1. Cache the app shell (HTML scaffold, CSS, JS, icons) so the PWA installs cleanly. We do not cache `/api/*` or `/repos/*` or `/sessions/*`. A stale dashboard is worse than a connection error.
2. Handle `push` events: parse the JSON payload, call `registration.showNotification(title, options)`. The OS draws the toast.
3. Handle `notificationclick` events: focus the existing PWA window if open, otherwise open it at the URL embedded in the notification's `data` field.

The SW runs outside the page lifetime. When Chrome receives a push for our origin, the OS wakes the SW, runs the `push` handler, and lets it die again. No tab needs to be open.

### Web manifest

`static/manifest.webmanifest`. Tells Android this is installable: name, short name, theme color, 192/512 PNG icons, `display: standalone`, `start_url: /`. Without this, "Add to Home Screen" gives you a glorified bookmark instead of a real PWA.

### Push dispatcher

Runs on the server, after each scan, in the same task that already does the local + remote refresh.

```
1. Build the new action-required set from this scan.
2. Subtract the seen_signals set. The remainder is "new since last scan."
3. For each new id, for each subscription:
     - Render a payload: { title, body, url, signal_id }
     - Encrypt it with the subscription's keys (aes128gcm, http-ece).
     - POST to the subscription's endpoint with VAPID JWT auth.
4. Add the new ids to seen_signals.
5. Optionally: prune from seen_signals any ids that no longer appear in
   the action-required set. This re-arms them so the next failure
   re-notifies. Without this step, "broken, fixed, broken again"
   only notifies once.
```

Crate: [`web-push`](https://crates.io/crates/web-push) handles VAPID JWT signing + http-ece encryption + POSTing to FCM. We do not implement the protocol ourselves.

### Frontend JS

`static/push-subscribe.js`. Loaded on the dashboard. Three states:

- `Notification.permission === "default"`: show an "enable notifications" button in the header. Click triggers `Notification.requestPermission()`. Browsers only allow that call inside a user gesture, so it must be a click handler, not page-load code.
- `=== "granted"`: subscribe via `registration.pushManager.subscribe({applicationServerKey: <VAPID_PUB>, userVisibleOnly: true})`, POST the resulting subscription to `/api/push/subscribe`. Hide the button.
- `=== "denied"`: hide the button. There is no programmatic re-prompt. The user has to clear the site's notification permission in Chrome settings to come back.

`userVisibleOnly: true` is required by Chrome. It is a promise: every push we receive must result in a visible notification, no silent background work. Violating it causes Chrome to revoke the subscription.

## End-to-end flow

```
[ install ]

  Phone ──open──> https://kai-server.<tailnet>.ts.net
  Page  ──register──> service worker
  User  ──tap "enable notifications"
  Page  ──Notification.requestPermission()──> Chrome
  User  ──allow
  Page  ──pushManager.subscribe(VAPID_PUB)──> Chrome ──> FCM
  FCM   ──> Chrome ──> Page: PushSubscription { endpoint, keys }
  Page  ──POST /api/push/subscribe──> repo-recall
  repo-recall ──insert──> state.redb

[ steady state, no broken repos ]

  ... silence ...

[ CI on some repo flips to failing ]

  Background scan ──> action-required set grows by one new id
  Dispatcher diffs: new id "repo_42:ci_failing"
  For each subscription:
    web-push crate ──signs JWT──> POSTs encrypted payload ──> FCM endpoint
  FCM ──relays──> device
  Android wakes the service worker
  SW push handler ──> showNotification("repo-recall", "CI failing on foo-bar")
  OS ──> toast on the lockscreen

[ user taps the toast ]

  Android wakes the PWA
  SW notificationclick handler ──> clients.openWindow("/repos/42")
  User lands on the repo page

[ CI goes green again ]

  Next scan: id "repo_42:ci_failing" no longer in action-required.
  Dispatcher prunes it from seen_signals so a future failure re-notifies.
```

## Why each constraint exists

- **Why a vendor push service.** You cannot self-host the relay. FCM only accepts pushes signed with a VAPID key matching the one the browser registered with. The browser only accepts pushes that came through FCM. There is no "send a notification straight from kai-server to the phone" path. This is not a Google policy choice, it is the protocol.
- **Why VAPID.** Without it, FCM would accept pushes for any subscription from anyone, which would let randoms spam the device. VAPID lets the browser say "only accept pushes signed by this public key" at subscribe time.
- **Why a separate state DB.** The cache DB is wipe-on-restart by design (no migrations, no stale-state bugs, see `AGENTS.md`). Push subscriptions and the VAPID keypair must survive restarts or every reboot kills push.
- **Why dedup by `<repo_id>:<signal>` instead of scan_version.** `scan_version` resets when the cache DB is rebuilt. If we keyed dedup on it, every server restart would re-notify every currently-broken repo. The id pair is stable across restarts.
- **Why no API caching in the SW.** A dashboard cached at "yesterday's truth" is misleading in a way that "the server is unreachable" is not. Connection error is honest, stale data is a lie.
- **Why Android-only.** iOS Safari supports Web Push for installed PWAs since 16.4, and the protocol is the same, but validating the iOS path is its own work and Kai does not have an iOS device to test on. The codepath generalizes if iOS gets added later.

## Privacy notes

- The push payload contains the repo name and signal type. It does not contain code, commits, or session content.
- FCM sees the encrypted payload and the destination device. Google can correlate "this server pushes to this device at these times" but cannot decrypt the body. This is the same posture as any Web Push deployment.
- The README's "one outbound call (`gh run list`)" claim must be amended once dispatch ships. Outbound calls become: `gh run list` (CI status) and `POST <fcm endpoint>` per push delivery.
- The state DB at `~/.local/share/repo-recall/state.redb` is sensitive: anyone with read access can impersonate the server (private VAPID key) or send notifications to subscribed devices (subscription rows). It is mode 0600 by default. Treat it like an SSH private key.

## References

- [Web Push protocol](https://datatracker.ietf.org/doc/html/rfc8030)
- [Message Encryption for Web Push](https://datatracker.ietf.org/doc/html/rfc8291) (http-ece, aes128gcm)
- [VAPID](https://datatracker.ietf.org/doc/html/rfc8292)
- [`web-push` crate](https://crates.io/crates/web-push)
- [MDN: Push API](https://developer.mozilla.org/en-US/docs/Web/API/Push_API)
- [MDN: Notifications API](https://developer.mozilla.org/en-US/docs/Web/API/Notifications_API)
- [Chrome's `userVisibleOnly` requirement](https://chromestatus.com/feature/5705087019683840)
