export type Repo = {
  id: number;
  path: string;
  name: string;
  session_count: number;
  commits_30d: number;
  loc_churn_30d: number;
  untracked_files: number;
  modified_files: number;
  authors_30d: number;
  commits_ahead: number;
  commits_behind: number;
  stash_count: number;
  head_ref: string | null;
  in_progress_op: string | null;
  open_prs: number;
  draft_prs: number;
  open_issues: number;
  prs_awaiting_my_review: number;
  prs_mine_awaiting_review: number;
  prs_mine_no_reviewer: number;
  my_draft_prs: number;
  issues_assigned_to_me: number;
  remote_url: string | null;
  default_branch: string | null;
  action_required: boolean;
  action_signals: string[];
  activity_score: number;
};

export type ActionRequiredItem = {
  id: string;
  repo_id: number;
  repo_name: string;
  repo_path: string;
  remote_url: string | null;
  signal: string;
  detail: string | null;
};

export type SessionRow = {
  id: number;
  session_uuid: string;
  last_prompt: string | null;
  started_at: number | null;
  ended_at: number | null;
  message_count: number;
  duration_ms: number | null;
  repos: { id: number; name: string }[];
};

export type CommitRow = {
  id: number;
  sha: string;
  subject: string;
  author_name: string | null;
  author_email: string | null;
  committed_at: number;
  repo_id: number;
  repo_name: string;
};

export type AutonomyMetrics = {
  overall: { total: number; success: number; failure: number; pending: number };
  overall_success_rate: number;
  per_repo: { repo_id: number; repo_name: string; total: number; success: number; failure: number; pending: number; success_rate: number }[];
};

export type StructuralAsk = {
  repo_id: number;
  repo_name: string;
  number: number;
  title: string;
  url: string;
  updated_at: number | null;
};

export type Dashboard = {
  repos: Repo[];
  recent_sessions: SessionRow[];
  recent_commits: CommitRow[];
  action_required: ActionRequiredItem[];
  autonomy: AutonomyMetrics;
  structural_asks: StructuralAsk[];
  counts: { repos: number; sessions: number; links: number; commits: number };
  gh_health: string;
  last_scan: number | null;
  scan_version: number;
  generated_at: number;
};
