// Reload the dashboard when the server bumps scan_version. Polls
// /api/scan-version on a slow cadence (cheap endpoint, just an integer).
//
// Scoped to the dashboard only: detail pages don't include this script,
// so they won't reload mid-read.
(function () {
  const POLL_MS = 5000;
  let last = null;
  async function tick() {
    try {
      const res = await fetch("/api/scan-version", { cache: "no-store" });
      if (!res.ok) return;
      const body = await res.json();
      const v = body && typeof body.scan_version === "number" ? body.scan_version : null;
      if (v === null) return;
      if (last === null) {
        last = v;
      } else if (v > last) {
        location.reload();
      }
    } catch (_) {}
  }
  setInterval(tick, POLL_MS);
  tick();
})();
