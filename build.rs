use std::process::Command;

// Resolves the version string baked into the binary, in priority order:
//   1. $REPO_RECALL_VERSION  - set by release CI and by the brew Formula
//   2. `git describe --tags` - dev builds from a clone with tags
//   3. Cargo.toml's pinned `0.0.0-dev` - last-resort fallback
//
// Cargo.toml's version field stays at `0.0.0-dev` permanently. Releases tag
// without committing back to main, so there's no version-bump commit to bury
// CI status from the prior commit.
fn main() {
    println!("cargo:rerun-if-env-changed=REPO_RECALL_VERSION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/tags");

    let version = if let Ok(v) = std::env::var("REPO_RECALL_VERSION") {
        if !v.is_empty() {
            v
        } else {
            git_describe().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
        }
    } else {
        git_describe().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
    };

    println!("cargo:rustc-env=REPO_RECALL_VERSION={version}");
}

fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        return None;
    }
    Some(s.strip_prefix('v').unwrap_or(&s).to_string())
}
