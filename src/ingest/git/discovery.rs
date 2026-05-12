use std::path::{Path, PathBuf};

use anyhow::Result;
use walkdir::WalkDir;

const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", ".venv", "venv"];

#[derive(Debug, Clone)]
pub struct DiscoveredRepo {
    pub path: PathBuf,
    pub name: String,
}

/// Scan `root` and up to `max_depth` levels down for directories containing a
/// `.git` entry (file or directory — worktrees use a file).
///
/// Depth 0 = root itself; depth 1 = immediate children; etc.
pub fn scan(root: &Path, max_depth: usize) -> Result<Vec<DiscoveredRepo>> {
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // walkdir handles depth + ordering; we drive descent control via
    // `it.skip_current_dir()` so a found repo doesn't recurse into its own
    // submodules / vendor git dirs.
    let mut it = WalkDir::new(&root)
        .min_depth(0)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter();
    while let Some(next) = it.next() {
        let entry = match next {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("walkdir error: {e}");
                continue;
            }
        };
        if !entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();
        let depth = entry.depth();
        let name = entry.file_name().to_string_lossy();

        // Skip noisy build/vendor dirs and hidden dirs (root is allowed even
        // if hidden — `is_root` semantics from the old hand-roll).
        if depth > 0 && (SKIP_DIRS.contains(&name.as_ref()) || name.starts_with('.')) {
            it.skip_current_dir();
            continue;
        }

        if path.join(".git").exists() {
            let canon = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            if seen.insert(canon.clone()) {
                let display_name = canon
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| canon.display().to_string());
                out.push(DiscoveredRepo {
                    path: canon,
                    name: display_name,
                });
            }
            // Found a repo — don't descend into it.
            it.skip_current_dir();
        }
    }
    Ok(out)
}
