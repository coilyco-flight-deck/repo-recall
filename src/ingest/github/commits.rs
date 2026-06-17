//! Remote commit ingest (#109). Forgejo and GitHub mirror the same REST
//! commit shape, so one parser maps both into `CommitRecord`.

use chrono::DateTime;

use crate::ingest::git::log::CommitRecord;

/// Pure parser for a GitHub/Forgejo `GET /repos/X/commits` array, mapped onto
/// the same `CommitRecord` the local `git log` scanner emits (#109).
pub fn parse_commits_json(value: &serde_json::Value) -> Vec<CommitRecord> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for c in arr {
        let sha = c.get("sha").and_then(|v| v.as_str()).unwrap_or("");
        if sha.is_empty() {
            continue;
        }
        let commit = c.get("commit");
        let author = commit.and_then(|v| v.get("author"));
        let committer = commit.and_then(|v| v.get("committer"));
        let message = commit
            .and_then(|v| v.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // `git log` records the subject (%s) as the first line and the body
        // (%B) as the whole message; mirror that split here.
        let subject = message.lines().next().unwrap_or("").to_string();
        let author_date = sub_str(author, "date");
        let parents = c
            .get("parents")
            .and_then(|v| v.as_array())
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| p.get("sha").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        out.push(CommitRecord {
            sha: sha.to_string(),
            author_name: sub_str(author, "name"),
            author_email: sub_str(author, "email"),
            timestamp: parse_ts(&author_date),
            subject,
            committer_name: sub_str(committer, "name"),
            committer_email: sub_str(committer, "email"),
            committer_date_iso: sub_str(committer, "date"),
            parents,
            // Decorated refs (%D) are a working-copy notion; the commits API
            // has no equivalent, so remote-ingested commits carry none.
            refs: String::new(),
            body: message.to_string(),
        });
    }
    out
}

fn sub_str(obj: Option<&serde_json::Value>, key: &str) -> String {
    obj.and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn parse_ts(iso: &str) -> i64 {
    DateTime::parse_from_rfc3339(iso)
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_forgejo_commit_array() {
        let body = serde_json::json!([
            {
                "sha": "e3770ffdd719ad3979f058f401d7a1bc8ab7ddf3",
                "commit": {
                    "author": { "name": "Kai Siren", "email": "coilysiren@gmail.com", "date": "2026-06-17T11:48:08Z" },
                    "committer": { "name": "Kai Siren", "email": "coilysiren@gmail.com", "date": "2026-06-17T11:48:08Z" },
                    "message": "chore(formula): bump to v0.48.0 [skip ci]\nbody line"
                },
                "parents": [ { "sha": "ea1136553c04b59d550fcaef5d4cbc79507fe4ed" } ]
            },
            { "commit": { "message": "no sha -> skipped" } }
        ]);
        let parsed = parse_commits_json(&body);
        assert_eq!(parsed.len(), 1);
        let c = &parsed[0];
        assert_eq!(c.sha, "e3770ffdd719ad3979f058f401d7a1bc8ab7ddf3");
        assert_eq!(c.author_name, "Kai Siren");
        assert_eq!(c.subject, "chore(formula): bump to v0.48.0 [skip ci]");
        assert!(c.body.contains("body line"));
        assert_eq!(c.parents, "ea1136553c04b59d550fcaef5d4cbc79507fe4ed");
        assert!(c.timestamp > 0);
        assert!(c.refs.is_empty());
    }
}
