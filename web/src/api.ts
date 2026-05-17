import type { Dashboard } from "./types";

export async function fetchDashboard(): Promise<Dashboard> {
  const res = await fetch("/", { headers: { accept: "application/json" } });
  if (!res.ok) throw new Error(`dashboard ${res.status}`);
  return (await res.json()) as Dashboard;
}

export async function fetchScanVersion(): Promise<number> {
  const res = await fetch("/api/scan-version");
  if (!res.ok) throw new Error(`scan-version ${res.status}`);
  const body = (await res.json()) as { scan_version: number };
  return body.scan_version;
}
