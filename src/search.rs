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

/// One indexed document. `kind`/`ref_id`/`text` mirror the original FTS5
/// row shape. `turn` is set only for `kind = "session_turn"` docs (#229):
/// one document per session turn, so a hit lands on the exact prompt,
/// model output, or thinking step rather than the whole chat.
#[derive(Debug, Clone)]
pub struct IndexDoc {
    pub kind: String,
    pub ref_id: i64,
    pub text: String,
    pub turn: Option<TurnPointer>,
}

impl IndexDoc {
    /// A repo / session / commit doc with no turn pointer.
    pub fn plain(kind: impl Into<String>, ref_id: i64, text: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            ref_id,
            text: text.into(),
            turn: None,
        }
    }
}

/// Pointer back to one turn of a session transcript. Stored alongside the
/// indexed text so a `session_turn` hit can pivot straight to the turn.
#[derive(Debug, Clone)]
pub struct TurnPointer {
    /// The session this turn belongs to.
    pub session_uuid: String,
    /// 0-based position of the turn within the parsed transcript.
    pub turn_index: i64,
    /// `user`, `assistant`, or `system`.
    pub turn_role: String,
}

/// Top-K hit. Same shape as `db::SearchHit` minus the snippet (snippet is
/// not used in step 2 - reader still flows through SQLite).
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub kind: String,
    pub ref_id: i64,
    pub text: String,
    pub score: f32,
    pub turn: Option<TurnPointer>,
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
    session_uuid: Field,
    turn_index: Field,
    turn_role: Field,
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
        // Turn-pointer fields for `session_turn` docs (#229). Stored so a
        // hit can pivot to the exact turn; not tokenized — they are
        // identifiers, not searchable prose.
        let session_uuid = builder.add_text_field("session_uuid", STRING | STORED);
        let turn_index = builder.add_i64_field("turn_index", STORED);
        let turn_role = builder.add_text_field("turn_role", STRING | STORED);
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
                fields: Fields {
                    kind,
                    ref_id,
                    text,
                    session_uuid,
                    turn_index,
                    turn_role,
                },
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
            if let Some(t) = &doc.turn {
                td.add_text(f.session_uuid, &t.session_uuid);
                td.add_i64(f.turn_index, t.turn_index);
                td.add_text(f.turn_role, &t.turn_role);
            }
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
            // A `session_turn` doc carries a turn pointer; everything else
            // leaves `turn` as `None`.
            let turn = doc
                .get_first(f.session_uuid)
                .and_then(|v| v.as_str())
                .map(|uuid| TurnPointer {
                    session_uuid: uuid.to_string(),
                    turn_index: doc
                        .get_first(f.turn_index)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    turn_role: doc
                        .get_first(f.turn_role)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            out.push(SearchHit {
                kind,
                ref_id,
                text,
                score,
                turn,
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
            IndexDoc::plain("session", 1, "Tracked down the CI flake in the auth tests"),
            IndexDoc::plain("commit", 42, "fix: handle empty cwd in scanner"),
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
        idx.rebuild(vec![IndexDoc::plain("session", 1, "alpha bravo")])
            .unwrap();
        assert_eq!(idx.search("alpha", 10).unwrap().len(), 1);

        idx.rebuild(vec![IndexDoc::plain("session", 2, "charlie delta")])
            .unwrap();
        assert!(idx.search("alpha", 10).unwrap().is_empty());
        assert_eq!(idx.search("charlie", 10).unwrap().len(), 1);
    }

    #[test]
    fn session_turn_doc_round_trips_its_turn_pointer() {
        let dir = temp_dir();
        let idx = SearchIndex::open_at(&dir).unwrap();
        idx.rebuild(vec![
            IndexDoc {
                kind: "session_turn".into(),
                ref_id: 7,
                text: "redact the transcript before indexing it".into(),
                turn: Some(TurnPointer {
                    session_uuid: "abc-123".into(),
                    turn_index: 4,
                    turn_role: "assistant".into(),
                }),
            },
            IndexDoc::plain("commit", 9, "unrelated commit subject"),
        ])
        .unwrap();

        let hits = idx.search("transcript", 10).unwrap();
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.kind, "session_turn");
        assert_eq!(hit.ref_id, 7);
        let turn = hit.turn.as_ref().expect("turn pointer present");
        assert_eq!(turn.session_uuid, "abc-123");
        assert_eq!(turn.turn_index, 4);
        assert_eq!(turn.turn_role, "assistant");

        // A plain doc leaves the turn pointer empty.
        let plain = idx.search("unrelated", 10).unwrap();
        assert_eq!(plain.len(), 1);
        assert!(plain[0].turn.is_none());
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
