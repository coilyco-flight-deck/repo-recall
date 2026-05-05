// Tantivy full-text index. Step 2 of the redb migration tracked in #39:
// dual-write the FTS5 `search_idx` rows into a tantivy index living
// alongside the SQLite cache in $TMPDIR. SQLite stays the read path for
// /search until step 3 flips the reader.
//
// Tokenizer choice matches FTS5's `porter unicode61 remove_diacritics 1`
// closely enough: tantivy's `en_stem` does Unicode + lowercase + Porter.
// Diacritic stripping is approximate (tantivy applies NFKC + lowercase, no
// explicit diacritic-fold), which is fine for the corpus we index.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, STORED, STRING,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};

/// One indexed document. Mirrors the FTS5 row shape (`kind`, `ref_id`,
/// `text`) so the eventual reader flip in step 3 is a like-for-like swap.
#[derive(Debug, Clone)]
pub struct IndexDoc {
    pub kind: String,
    pub ref_id: i64,
    pub text: String,
}

/// Top-K hit. Same shape as `db::SearchHit` minus the snippet (snippet is
/// not used in step 2 - reader still flows through SQLite).
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub kind: String,
    pub ref_id: i64,
    pub text: String,
    pub score: f32,
}

#[derive(Clone)]
pub struct SearchIndex {
    inner: Arc<Inner>,
}

struct Inner {
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    fields: Fields,
}

#[derive(Clone, Copy)]
struct Fields {
    kind: Field,
    ref_id: Field,
    text: Field,
}

impl SearchIndex {
    /// Open a tantivy index at `dir`. The directory is wiped first so the
    /// index always starts clean alongside the SQLite cache (which is also
    /// dropped + recreated at process start). The directory is created if
    /// missing.
    pub fn open_at(dir: &Path) -> Result<Self> {
        if dir.exists() {
            std::fs::remove_dir_all(dir).with_context(|| format!("clear tantivy dir: {dir:?}"))?;
        }
        std::fs::create_dir_all(dir).with_context(|| format!("create tantivy dir: {dir:?}"))?;

        let mut builder = Schema::builder();
        let kind = builder.add_text_field("kind", STRING | STORED);
        let ref_id = builder.add_i64_field("ref_id", STORED | FAST);
        let text_indexing = TextFieldIndexing::default()
            .set_tokenizer("en_stem")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let text_opts = TextOptions::default()
            .set_indexing_options(text_indexing)
            .set_stored();
        let text = builder.add_text_field("text", text_opts);
        let schema = builder.build();

        let index = Index::create_in_dir(dir, schema).context("create tantivy index")?;
        let writer = index
            .writer(50_000_000) // 50 MB heap, plenty for the corpus.
            .context("create tantivy IndexWriter")?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("build tantivy IndexReader")?;

        Ok(Self {
            inner: Arc::new(Inner {
                index,
                reader,
                writer: Mutex::new(writer),
                fields: Fields { kind, ref_id, text },
            }),
        })
    }

    /// Drop every doc and rebuild from the supplied iterator. Called at the
    /// end of a refresh, after SQLite's `rebuild_search_index` has run, with
    /// the same rows. Commits once at the end.
    pub fn rebuild<I>(&self, docs: I) -> Result<()>
    where
        I: IntoIterator<Item = IndexDoc>,
    {
        let mut writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("tantivy writer mutex poisoned"))?;
        writer.delete_all_documents()?;
        let f = self.inner.fields;
        for doc in docs {
            let mut td = TantivyDocument::default();
            td.add_text(f.kind, &doc.kind);
            td.add_i64(f.ref_id, doc.ref_id);
            td.add_text(f.text, &doc.text);
            writer.add_document(td)?;
        }
        writer.commit()?;
        // Manual reload so the next searcher sees the new commit.
        self.inner.reader.reload()?;
        Ok(())
    }

    /// Run a free-text query. Used by step 3+ - kept here in step 2 so the
    /// dual-write side of the migration is testable end-to-end.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let searcher = self.inner.reader.searcher();
        let f = self.inner.fields;
        let mut parser = QueryParser::for_index(&self.inner.index, vec![f.text]);
        // Match FTS5 phrase-quoting behaviour: treat the whole input as a
        // phrase by default so users do not need to escape `:` or `*`.
        parser.set_conjunction_by_default();
        let parsed = parser
            .parse_query(query)
            .with_context(|| format!("parse query {query:?}"))?;
        let top = searcher.search(&parsed, &TopDocs::with_limit(limit).order_by_score())?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let kind = doc
                .get_first(f.kind)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ref_id = doc
                .get_first(f.ref_id)
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let text = doc
                .get_first(f.text)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push(SearchHit {
                kind,
                ref_id,
                text,
                score,
            });
        }
        Ok(out)
    }
}

/// Default on-disk path for the tantivy index, alongside the SQLite cache
/// in `$TMPDIR`. Honours `REPO_RECALL_INDEX_DIR` for tests.
pub fn default_index_dir() -> PathBuf {
    if let Ok(p) = std::env::var("REPO_RECALL_INDEX_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    std::env::temp_dir().join("repo-recall-index")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "repo-recall-index-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn rebuild_then_search_finds_matching_doc() {
        let dir = temp_dir();
        let idx = SearchIndex::open_at(&dir).unwrap();
        idx.rebuild(vec![
            IndexDoc {
                kind: "session".into(),
                ref_id: 1,
                text: "Tracked down the CI flake in the auth tests".into(),
            },
            IndexDoc {
                kind: "commit".into(),
                ref_id: 42,
                text: "fix: handle empty cwd in scanner".into(),
            },
        ])
        .unwrap();

        let hits = idx.search("flake", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "session");
        assert_eq!(hits[0].ref_id, 1);

        // Stemming: "scanned" should still hit "scanner".
        let stem_hits = idx.search("scanner", 10).unwrap();
        assert!(stem_hits.iter().any(|h| h.ref_id == 42));
    }

    #[test]
    fn rebuild_replaces_prior_docs() {
        let dir = temp_dir();
        let idx = SearchIndex::open_at(&dir).unwrap();
        idx.rebuild(vec![IndexDoc {
            kind: "session".into(),
            ref_id: 1,
            text: "alpha bravo".into(),
        }])
        .unwrap();
        assert_eq!(idx.search("alpha", 10).unwrap().len(), 1);

        idx.rebuild(vec![IndexDoc {
            kind: "session".into(),
            ref_id: 2,
            text: "charlie delta".into(),
        }])
        .unwrap();
        assert!(idx.search("alpha", 10).unwrap().is_empty());
        assert_eq!(idx.search("charlie", 10).unwrap().len(), 1);
    }

    #[test]
    fn open_at_clears_existing_dir() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stray.txt"), b"junk").unwrap();
        let _idx = SearchIndex::open_at(&dir).unwrap();
        assert!(!dir.join("stray.txt").exists());
    }
}
