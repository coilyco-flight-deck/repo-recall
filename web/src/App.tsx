import { useCallback, useEffect, useMemo, useState } from "react";
import { fetchDashboard, fetchScanVersion } from "./api";
import type { Dashboard, Repo } from "./types";
import { RepoCard } from "./RepoCard";

const SCAN_POLL_MS = 10_000;

function readExpanded(): number | null {
  const sp = new URLSearchParams(window.location.search);
  const v = sp.get("expanded");
  if (!v) return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

function writeExpanded(id: number | null) {
  const sp = new URLSearchParams(window.location.search);
  if (id === null) sp.delete("expanded");
  else sp.set("expanded", String(id));
  const qs = sp.toString();
  const url = qs ? `${window.location.pathname}?${qs}` : window.location.pathname;
  window.history.replaceState(null, "", url);
}

function sortRepos(repos: Repo[]): Repo[] {
  return [...repos].sort((a, b) => {
    if (a.action_required !== b.action_required) return a.action_required ? -1 : 1;
    if (a.activity_score !== b.activity_score) return b.activity_score - a.activity_score;
    return a.name.localeCompare(b.name);
  });
}

export default function App() {
  const [data, setData] = useState<Dashboard | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<number | null>(() => readExpanded());
  const [filter, setFilter] = useState("");

  const refresh = useCallback(async () => {
    try {
      const d = await fetchDashboard();
      setData(d);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (!data) return;
    let lastSeen = data.scan_version;
    const id = window.setInterval(async () => {
      try {
        const v = await fetchScanVersion();
        if (v !== lastSeen) {
          lastSeen = v;
          refresh();
        }
      } catch {
        /* ignore poll blips */
      }
    }, SCAN_POLL_MS);
    return () => window.clearInterval(id);
  }, [data, refresh]);

  useEffect(() => {
    const onPop = () => setExpanded(readExpanded());
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const onToggle = useCallback((id: number | null) => {
    setExpanded(id);
    writeExpanded(id);
  }, []);

  const visibleRepos = useMemo(() => {
    if (!data) return [];
    const sorted = sortRepos(data.repos);
    const q = filter.trim().toLowerCase();
    if (!q) return sorted;
    return sorted.filter((r) => r.name.toLowerCase().includes(q) || (r.remote_url ?? "").toLowerCase().includes(q));
  }, [data, filter]);

  if (error) {
    return (
      <main className="min-h-screen flex items-center justify-center bg-slate-50 text-rose-700">
        <div>failed to load dashboard: {error}</div>
      </main>
    );
  }
  if (!data) {
    return (
      <main className="min-h-screen flex items-center justify-center bg-slate-50 text-slate-500">
        loading…
      </main>
    );
  }

  const expandedRepo = expanded !== null ? visibleRepos.find((r) => r.id === expanded) ?? null : null;

  return (
    <main className="min-h-screen bg-slate-50 text-slate-900 font-sans">
      <header className="px-6 py-4 border-b border-slate-200 bg-white sticky top-0 z-10 flex flex-wrap items-baseline gap-4">
        <h1 className="text-xl font-bold">repo-recall</h1>
        <div className="text-xs text-slate-500">
          {data.counts.repos} repos · {data.counts.sessions} sessions · {data.counts.commits} commits · scan{" "}
          {data.scan_version}
        </div>
        <input
          type="search"
          placeholder="filter repos…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          className="ml-auto px-2 py-1 text-sm border border-slate-300 rounded w-64 focus:outline-none focus:border-indigo-500"
        />
      </header>

      {expandedRepo && (
        <div
          onClick={() => onToggle(null)}
          className="fixed inset-0 bg-slate-900/10 backdrop-blur-sm z-20 cursor-zoom-out"
        />
      )}

      <div className="grid gap-4 p-4 grid-cols-1 md:grid-cols-2 min-[1920px]:grid-cols-3 relative">
        {expandedRepo && (
          <div className="fixed inset-0 z-30 flex items-start justify-center overflow-auto p-6 pointer-events-none">
            <div className="w-full max-w-3xl pointer-events-auto">
              <RepoCard
                repo={expandedRepo}
                expanded={true}
                faded={false}
                onToggle={onToggle}
                recentCommits={data.recent_commits}
                recentSessions={data.recent_sessions}
                actions={data.action_required}
              />
            </div>
          </div>
        )}
        {visibleRepos.map((repo) => (
          <RepoCard
            key={repo.id}
            repo={repo}
            expanded={false}
            faded={expanded !== null && expanded !== repo.id}
            onToggle={onToggle}
            recentCommits={data.recent_commits}
            recentSessions={data.recent_sessions}
            actions={data.action_required}
          />
        ))}
      </div>
    </main>
  );
}
