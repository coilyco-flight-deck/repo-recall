import { useState } from "react";
import type { Repo, CommitRow, SessionRow, ActionRequiredItem } from "./types";

type Props = {
  repo: Repo;
  expanded: boolean;
  faded: boolean;
  onToggle: (id: number | null) => void;
  recentCommits: CommitRow[];
  recentSessions: SessionRow[];
  actions: ActionRequiredItem[];
};

const Row = ({ label, children }: { label: string; children: React.ReactNode }) => (
  <div className="flex gap-3 text-sm leading-tight py-0.5">
    <div className="w-24 shrink-0 text-slate-500">{label}</div>
    <div className="flex-1 text-slate-800">{children}</div>
  </div>
);

const slugFromRemote = (url: string | null): string | null => {
  if (!url) return null;
  const m = url.match(/github\.com[:/](.+?)(?:\.git)?$/);
  return m ? m[1] : null;
};

/// Session text (#229) is obscured by default. Revealing is a deliberate
/// click — on a tailnet-hosted instance the blur is the only thing between
/// a prompt and a passer-by. Stops click propagation so revealing a prompt
/// doesn't also collapse the card.
function RedactedText({ text }: { text: string }) {
  const [revealed, setRevealed] = useState(false);
  if (revealed) return <span className="text-slate-700">{text}</span>;
  return (
    <span
      onClick={(e) => {
        e.stopPropagation();
        setRevealed(true);
      }}
      title="click to reveal session text"
      className="text-slate-700 blur-[3px] hover:blur-[2px] select-none cursor-pointer transition-all"
    >
      {text}
    </span>
  );
}

const fmtAgo = (ts: number | null): string => {
  if (!ts) return "—";
  const s = Math.floor(Date.now() / 1000) - ts;
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
};

export function RepoCard({ repo, expanded, faded, onToggle, recentCommits, recentSessions, actions }: Props) {
  const slug = slugFromRemote(repo.remote_url);
  const myCommits = recentCommits.filter((c) => c.repo_id === repo.id);
  const mySessions = recentSessions.filter((s) => s.repos.some((r) => r.id === repo.id));
  const myActions = actions.filter((a) => a.repo_id === repo.id);

  const commitCap = expanded ? 30 : 5;
  const sessionCap = expanded ? 30 : 5;

  return (
    <article
      onClick={() => onToggle(expanded ? null : repo.id)}
      className={[
        "rounded-lg border border-slate-200 bg-white shadow-sm p-4 cursor-pointer transition-all duration-300",
        faded ? "opacity-0 pointer-events-none" : "opacity-100 hover:border-slate-300",
        expanded ? "ring-2 ring-indigo-300" : "",
        repo.action_required ? "border-l-4 border-l-rose-500" : "",
      ].join(" ")}
    >
      <header className="flex items-baseline justify-between gap-2 mb-2">
        <h2 className="text-lg font-semibold text-slate-900 truncate">{repo.name}</h2>
        {slug && (
          <a
            href={`https://github.com/${slug}`}
            onClick={(e) => e.stopPropagation()}
            className="text-xs font-mono text-slate-500 hover:text-indigo-700 truncate"
          >
            {slug}
          </a>
        )}
      </header>

      {repo.action_required && myActions.length > 0 && (
        <div className="mb-2 text-xs text-rose-700 bg-rose-50 border border-rose-200 rounded px-2 py-1">
          {myActions.slice(0, expanded ? 20 : 3).map((a) => (
            <div key={a.id}>
              <span className="font-medium">{a.signal}</span>
              {a.detail && <span className="text-rose-600"> - {a.detail}</span>}
            </div>
          ))}
        </div>
      )}

      <Row label="health">
        <span>
          dirty: {repo.untracked_files + repo.modified_files}
        </span>
        {repo.in_progress_op && <span className="ml-3 text-amber-700">op: {repo.in_progress_op}</span>}
        {repo.head_ref === "detached" && <span className="ml-3 text-amber-700">detached</span>}
      </Row>

      <Row label="activity">
        {repo.commits_30d} commits / 30d · {repo.authors_30d} authors · score {repo.activity_score.toFixed(2)}
      </Row>

      <Row label="work">
        {repo.open_prs} PRs ({repo.draft_prs} draft) · {repo.open_issues} issues
        {repo.prs_awaiting_my_review > 0 && (
          <span className="ml-2 text-indigo-700">· {repo.prs_awaiting_my_review} awaiting me</span>
        )}
      </Row>

      <Row label="asks">
        {repo.issues_assigned_to_me} assigned · {repo.prs_mine_awaiting_review} mine waiting
      </Row>

      <Row label="sessions">{repo.session_count} total</Row>

      <Row label="churn">
        {repo.loc_churn_30d.toLocaleString()} loc / 30d
        {repo.commits_ahead > 0 && <span className="ml-2">↑{repo.commits_ahead}</span>}
        {repo.commits_behind > 0 && <span className="ml-2">↓{repo.commits_behind}</span>}
        {repo.stash_count > 0 && <span className="ml-2">stash:{repo.stash_count}</span>}
      </Row>

      {expanded && (
        <div className="mt-3 border-t border-slate-200 pt-3 space-y-3">
          <div>
            <div className="text-xs font-semibold text-slate-500 mb-1">recent commits</div>
            {myCommits.length === 0 ? (
              <div className="text-xs text-slate-400">none</div>
            ) : (
              <ul className="text-xs space-y-0.5">
                {myCommits.slice(0, commitCap).map((c) => (
                  <li key={c.id} className="font-mono truncate">
                    <span className="text-slate-400">{c.sha.slice(0, 7)}</span>{" "}
                    <span className="text-slate-700">{c.subject}</span>{" "}
                    <span className="text-slate-400">{fmtAgo(c.committed_at)}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <div>
            <div className="text-xs font-semibold text-slate-500 mb-1">recent sessions</div>
            {mySessions.length === 0 ? (
              <div className="text-xs text-slate-400">none</div>
            ) : (
              <ul className="text-xs space-y-0.5">
                {mySessions.slice(0, sessionCap).map((s) => (
                  <li key={s.id} className="truncate">
                    {s.last_prompt ? (
                      <RedactedText text={s.last_prompt} />
                    ) : (
                      <span className="text-slate-700">{s.session_uuid.slice(0, 8)}</span>
                    )}{" "}
                    <span className="text-slate-400">{fmtAgo(s.ended_at ?? s.started_at)}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <div className="text-xs text-slate-400 font-mono truncate">{repo.path}</div>
        </div>
      )}
    </article>
  );
}
