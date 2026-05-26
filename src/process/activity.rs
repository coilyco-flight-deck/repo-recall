//! Activity scoring for repos — one number summarising "how lively is this
//! place" across every activity dimension we've wired up. Used to sort the

use crate::db::Repo;

/// Function that pulls one activity dimension's value off a repo.
pub type AttrFn = fn(&Repo) -> i64;

/// Three categories of repo signal, each with its own pace and cost:
///
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Historical,
    LocalState,
    RemoteState,
}

/// One activity dimension: a stable key (for logs / future debugging), its
/// category (see [`Category`]), and the function that extracts its per-repo
pub struct Attr {
    pub key: &'static str,
    pub category: Category,
    pub get: AttrFn,
}

/// Every activity dimension we've wired up. The design target is 5+ entries.
/// To add a new dimension: land the underlying count on `Repo`, then append
pub const ATTRS: &[Attr] = &[
    // --- Historical: past activity, cheap + offline --------------------
    Attr {
        key: "sessions",
        category: Category::Historical,
        get: |r| r.session_count,
    },
    Attr {
        key: "commits_30d",
        category: Category::Historical,
        get: |r| r.commits_30d,
    },
    Attr {
        key: "loc_churn_30d",
        category: Category::Historical,
        get: |r| r.loc_churn_30d,
    },
    Attr {
        key: "authors_30d",
        category: Category::Historical,
        get: |r| r.authors_30d,
    },
    // --- LocalState: current working tree, cheap + offline -------------
    Attr {
        // Combined signal — untracked + modified treated as one "working
        // tree is dirty" dimension. The breakdown still lives on `Repo`
        key: "uncommitted_files",
        category: Category::LocalState,
        get: |r| r.untracked_files + r.modified_files,
    },
    // --- RemoteState: requires a network call --------------------------
    Attr {
        key: "prs_awaiting_my_review",
        category: Category::RemoteState,
        get: |r| r.prs_awaiting_my_review,
    },
    Attr {
        // Open issues assigned to the authenticated user — the "what's on my
        // plate today" signal. Action-required when > 0.
        key: "issues_assigned_to_me",
        category: Category::RemoteState,
        get: |r| r.issues_assigned_to_me,
    },
    Attr {
        // Binary: 1 if the deploy workflow's last run on the default branch
        // failed. Mirrors `ci_failing`.
        key: "deploy_failing",
        category: Category::RemoteState,
        get: |r| i64::from(is_deploy_failing(r)),
    },
    Attr {
        // Binary: 1 if the deploy workflow had a green run before going quiet
        // and the most recent green run is more than 7 days old.
        key: "deploy_stale",
        category: Category::RemoteState,
        get: |r| i64::from(is_deploy_stale(r)),
    },
    Attr {
        // Not included in action-required (open PRs are informational), but
        // contributes to activity scoring so repos with active PR flow rank.
        key: "open_prs",
        category: Category::RemoteState,
        get: |r| r.open_prs,
    },
    Attr {
        // Count of stale local branches - unmerged work whose tip commit is
        // older than `STALE_BRANCH_SECS`. Action-required when > 0.
        key: "stale_branches",
        category: Category::LocalState,
        get: |r| r.stale_branches.len() as i64,
    },
];

/// Per-attribute normaliser across a slice of repos. Uses the **median of
/// non-zero values** rather than the max. Rationale: one super-active repo
pub fn normalisers(repos: &[Repo]) -> Vec<f64> {
    ATTRS
        .iter()
        .map(|a| {
            let mut vs: Vec<f64> = repos
                .iter()
                .map(|r| (a.get)(r) as f64)
                .filter(|v| *v > 0.0)
                .collect();
            if vs.is_empty() {
                return 0.0;
            }
            vs.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
            let n = vs.len();
            if n % 2 == 1 {
                vs[n / 2]
            } else {
                (vs[n / 2 - 1] + vs[n / 2]) / 2.0
            }
        })
        .collect()
}

/// Sum of `ln(1 + xᵢ / norm_i)` across every activity dimension. See the
/// module docs for the "why this shape". `norms` comes from [`normalisers`].
pub fn score(repo: &Repo, norms: &[f64]) -> f64 {
    ATTRS
        .iter()
        .zip(norms)
        .map(|(attr, &norm)| {
            if norm <= 0.0 {
                return 0.0;
            }
            let v = (attr.get)(repo) as f64;
            (1.0 + v / norm).ln()
        })
        .sum()
}

/// "Action required" repos — something tangible is waiting for you. These
/// hard-sort above the activity-score ranking.
pub const DEPLOY_STALE_SECS: i64 = 7 * 86_400;

/// Threshold for the `stale_branch` signal: a local branch whose tip commit
/// is older than this, and which is not merged into the default branch, is
pub const STALE_BRANCH_SECS: i64 = 24 * 3_600;

/// Current triggers:
/// - Dirty working tree (untracked + modified)
pub fn is_action_required(r: &Repo) -> bool {
    if is_vendored(r) {
        return false;
    }
    (r.untracked_files + r.modified_files) > 0
        || r.in_progress_op.is_some()
        || r.head_ref.as_deref() == Some("detached")
        || r.prs_awaiting_my_review > 0
        || r.my_draft_prs > 0
        || r.prs_mine_awaiting_review > 0
        || r.prs_mine_no_reviewer > 0
        || r.issues_assigned_to_me > 0
        || r.commits_ahead > 0
        || is_deploy_failing(r)
        || is_deploy_stale(r)
        || !r.stale_branches.is_empty()
}

/// Vendored / external repo: a third-party tree cloned for reading, not for
/// work. Suppresses every action-required signal so detached-HEAD release-tag
pub fn is_vendored(r: &Repo) -> bool {
    std::path::Path::new(&r.path)
        .join(".repo-recall-ignore")
        .exists()
}

/// Last run of the deploy workflow on the default branch failed.
pub fn is_deploy_failing(r: &Repo) -> bool {
    r.deploy_status.as_deref() == Some("failure")
}

/// Deploy workflow has gone quiet: there *was* a successful run, but the
/// most recent green run is older than [`DEPLOY_STALE_SECS`]. Repos that
pub fn is_deploy_stale(r: &Repo) -> bool {
    let Some(last_success) = r.deploy_last_success_ts else {
        return false;
    };
    // Already-failing supersedes stale - one signal at a time per repo.
    if is_deploy_failing(r) {
        return false;
    }
    let now = chrono::Utc::now().timestamp();
    now - last_success > DEPLOY_STALE_SECS
}

/// In-place sort. First by action-required (true before false), then by
/// activity score desc, with name as the final tiebreak.
pub fn sort(repos: &mut [Repo]) {
    let ns = normalisers(repos);
    repos.sort_by(|a, b| {
        is_action_required(b)
            .cmp(&is_action_required(a))
            .then_with(|| {
                let sa = score(a, &ns);
                let sb = score(b, &ns);
                sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// A repo is "dormant" when every activity dimension reads zero. Used for the
/// visual fade on the dashboard. Equivalent to `score(r) == 0` but doesn't
pub fn is_dormant(repo: &Repo) -> bool {
    ATTRS.iter().all(|a| (a.get)(repo) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(id: i64, name: &str, sessions: i64, commits: i64) -> Repo {
        Repo {
            id,
            name: name.into(),
            path: format!("/tmp/{name}"),
            session_count: sessions,
            commits_30d: commits,
            loc_churn_30d: 0,
            untracked_files: 0,
            modified_files: 0,
            authors_30d: 0,
            commits_ahead: 0,
            commits_behind: 0,
            stash_count: 0,
            head_ref: None,
            in_progress_op: None,
            stale_branches: Vec::new(),
            open_prs: 0,
            draft_prs: 0,
            open_issues: 0,
            prs_awaiting_my_review: 0,
            prs_mine_awaiting_review: 0,
            prs_mine_no_reviewer: 0,
            my_draft_prs: 0,
            issues_assigned_to_me: 0,
            deploy_workflow: None,
            deploy_status: None,
            deploy_last_success_ts: None,
            remote_url: None,
            default_branch: None,
        }
    }

    #[test]
    fn balanced_beats_lopsided() {
        // A repo with some activity in every dimension should rank above one
        // that's larger in a single dimension but zero in others.
        let mut repos = vec![
            repo(1, "lopsided", 0, 20),
            repo(2, "balanced", 5, 5),
            repo(3, "dormant", 0, 0),
        ];
        sort(&mut repos);
        assert_eq!(repos[0].name, "balanced");
        assert_eq!(repos[1].name, "lopsided");
        assert_eq!(repos[2].name, "dormant");
    }

    #[test]
    fn equal_scores_break_alphabetically() {
        let mut repos = vec![repo(1, "zulu", 0, 0), repo(2, "alpha", 0, 0)];
        sort(&mut repos);
        assert_eq!(repos[0].name, "alpha");
        assert_eq!(repos[1].name, "zulu");
    }

    #[test]
    fn zero_normaliser_does_not_panic() {
        // If every repo is zero on every dim, scores are all 0 and we fall
        // back to alpha ordering without div-by-zero panics.
        let ns = normalisers(&[repo(1, "a", 0, 0)]);
        assert!(score(&repo(1, "a", 0, 0), &ns).abs() < 1e-12);
    }

    #[test]
    fn median_gives_typical_repo_a_meaningful_score() {
        // Under max-normalisation, a solo outlier would squash everyone else
        // near zero. With median-normalisation, a "typical" repo (at the
        let repos = vec![
            repo(1, "solo-giant", 0, 1000),
            repo(2, "typical-a", 0, 10),
            repo(3, "typical-b", 0, 10),
            repo(4, "typical-c", 0, 10),
        ];
        let ns = normalisers(&repos);
        // Median of {1000, 10, 10, 10} = 10; score(typical) = ln(2).
        let typical = score(&repos[1], &ns);
        assert!((typical - 2f64.ln()).abs() < 1e-9, "got {typical}");
        // Outlier still scores higher than a typical repo, just not 100×.
        let outlier = score(&repos[0], &ns);
        assert!(outlier > typical);
    }

    #[test]
    fn in_progress_and_detached_head_trigger_action_required() {
        let mut rebasing = repo(1, "rebasing", 0, 0);
        rebasing.in_progress_op = Some("rebase".into());
        assert!(is_action_required(&rebasing));

        let mut detached = repo(2, "detached", 0, 0);
        detached.head_ref = Some("detached".into());
        assert!(is_action_required(&detached));

        let mut on_main = repo(3, "on-main", 0, 0);
        on_main.head_ref = Some("main".into());
        assert!(!is_action_required(&on_main));
    }

    #[test]
    fn unpushed_commits_trigger_action_required() {
        let mut clean = repo(1, "clean", 0, 0);
        clean.head_ref = Some("main".into());
        assert!(!is_action_required(&clean));

        let mut ahead = repo(2, "ahead", 0, 0);
        ahead.head_ref = Some("main".into());
        ahead.commits_ahead = 3;
        assert!(is_action_required(&ahead));

        // `commits_behind` stays informational - not a todo.
        let mut behind = repo(3, "behind", 0, 0);
        behind.head_ref = Some("main".into());
        behind.commits_behind = 4;
        assert!(!is_action_required(&behind));
    }

    #[test]
    fn stale_branches_trigger_action_required() {
        let mut clean = repo(1, "clean", 0, 0);
        clean.head_ref = Some("main".into());
        assert!(!is_action_required(&clean));

        let mut stale = repo(2, "stale", 0, 0);
        stale.head_ref = Some("main".into());
        stale.stale_branches = vec![crate::db::StaleBranch {
            name: "feature/old".into(),
            tip_age_secs: 3 * 86_400,
        }];
        assert!(is_action_required(&stale));
    }

    #[test]
    fn vendored_marker_silences_action_required() {
        let dir = std::env::temp_dir().join(format!(
            "repo-recall-vendored-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".repo-recall-ignore"), "").unwrap();

        let mut detached = repo(1, "vendored", 0, 0);
        detached.path = dir.to_string_lossy().to_string();
        detached.head_ref = Some("detached".into());
        detached.untracked_files = 5;
        assert!(is_vendored(&detached));
        assert!(!is_action_required(&detached));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dormant_detection() {
        assert!(is_dormant(&repo(1, "x", 0, 0)));
        assert!(!is_dormant(&repo(1, "x", 1, 0)));
        assert!(!is_dormant(&repo(1, "x", 0, 1)));
    }

    #[test]
    fn action_required_hard_sorts_to_top() {
        // A quiet repo with a dirty tree should outrank a very active repo
        // whose tree is clean.
        let mut noisy_clean = repo(1, "noisy-clean", 20, 500);
        noisy_clean.authors_30d = 15;

        let mut dormant_broken = repo(2, "dormant-broken", 0, 0);
        dormant_broken.in_progress_op = Some("rebase".into());
        dormant_broken.modified_files = 3;

        let mut quiet_dirty = repo(3, "quiet-dirty", 0, 0);
        quiet_dirty.modified_files = 3;

        let quiet_clean = repo(4, "quiet-clean", 0, 0);

        let mut repos = vec![
            noisy_clean.clone(),
            quiet_clean,
            dormant_broken.clone(),
            quiet_dirty.clone(),
        ];
        sort(&mut repos);

        // Both action-required repos come first (alpha within the bucket).
        assert!(is_action_required(&repos[0]));
        assert!(is_action_required(&repos[1]));
        assert_eq!(repos[0].name, "dormant-broken");
        assert_eq!(repos[1].name, "quiet-dirty");
        // Then the highest-activity repo, then the dormant-but-clean one.
        assert_eq!(repos[2].name, "noisy-clean");
        assert_eq!(repos[3].name, "quiet-clean");
    }
}
