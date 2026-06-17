//! Open-milestone ingest (#88). One parser, two shapes: Forgejo keys on `id`
//! (no `number`) and omits `html_url` (#109); GitHub uses `number`.

use chrono::DateTime;

use super::pulls::cap_body;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MilestoneInput {
    pub number: i64,
    pub title: String,
    pub description: String,
    pub html_url: String,
    pub state: String,
    pub due_on: Option<i64>,
    pub open_issues: i64,
    pub closed_issues: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
}

/// Pure parser for GitHub or Forgejo `milestones?state=open` JSON arrays.
pub fn parse_milestones_json(value: &serde_json::Value) -> Vec<MilestoneInput> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for m in arr {
        // GitHub uses a user-facing `number`; Forgejo (#109) keys on `id`
        // only. Prefer `number`, fall back to `id`, else drop the row.
        let number = m
            .get("number")
            .and_then(|v| v.as_i64())
            .or_else(|| m.get("id").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        if number == 0 {
            continue;
        }
        out.push(MilestoneInput {
            number,
            title: m
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            description: cap_body(m.get("description").and_then(|v| v.as_str()).unwrap_or("")),
            html_url: m
                .get("html_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            state: m
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("open")
                .to_string(),
            due_on: parse_opt_ts(m, "due_on"),
            open_issues: m.get("open_issues").and_then(|v| v.as_i64()).unwrap_or(0),
            closed_issues: m.get("closed_issues").and_then(|v| v.as_i64()).unwrap_or(0),
            created_at: parse_ts(m, "created_at"),
            updated_at: parse_ts(m, "updated_at"),
            closed_at: parse_opt_ts(m, "closed_at"),
        });
    }
    out
}

fn parse_ts(v: &serde_json::Value, key: &str) -> i64 {
    parse_opt_ts(v, key).unwrap_or(0)
}

fn parse_opt_ts(v: &serde_json::Value, key: &str) -> Option<i64> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_milestone_payload() {
        let body = serde_json::json!([
            {
                "number": 7,
                "title": "v1.0",
                "description": "First stable release.",
                "html_url": "https://github.com/coilyco-flight-deck/repo-recall/milestone/7",
                "state": "open",
                "due_on": "2026-06-30T07:00:00Z",
                "open_issues": 3,
                "closed_issues": 5,
                "created_at": "2026-05-01T12:00:00Z",
                "updated_at": "2026-05-20T08:00:00Z",
                "closed_at": serde_json::Value::Null,
            },
            {
                "number": 0,
                "title": "skipped: bad number"
            }
        ]);
        let parsed = parse_milestones_json(&body);
        assert_eq!(parsed.len(), 1);
        let m = &parsed[0];
        assert_eq!(m.number, 7);
        assert_eq!(m.title, "v1.0");
        assert_eq!(m.open_issues, 3);
        assert_eq!(m.closed_issues, 5);
        assert!(m.due_on.is_some());
        assert!(m.closed_at.is_none());
    }

    #[test]
    fn empty_array_yields_empty_vec() {
        let body = serde_json::json!([]);
        assert!(parse_milestones_json(&body).is_empty());
    }

    #[test]
    fn forgejo_milestone_keyed_on_id_not_number() {
        // Forgejo milestone payloads carry `id` but no `number` (#109). The
        // parser must fall back to `id` instead of dropping the row.
        let body = serde_json::json!([
            {
                "id": 5,
                "title": "coily to ward consolidation",
                "description": "epic slice",
                "state": "open",
                "open_issues": 0,
                "closed_issues": 31,
                "created_at": "2026-06-15T15:27:30Z",
                "updated_at": "2026-06-17T08:22:10Z",
                "closed_at": serde_json::Value::Null,
                "due_on": serde_json::Value::Null,
            }
        ]);
        let parsed = parse_milestones_json(&body);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].number, 5);
        assert_eq!(parsed[0].title, "coily to ward consolidation");
        assert_eq!(parsed[0].closed_issues, 31);
    }
}
