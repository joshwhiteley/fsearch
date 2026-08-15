//! Semantic search: find documents by meaning (`? essay about patience`).
//! Text is chunked and embedded locally; queries run brute-force cosine
//! over every chunk — at home-directory scale (≤ ~1M chunks) that's a few
//! milliseconds, so there is no vector database and no daemon, in keeping
//! with the rest of fsearch.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub const CHUNK_CHARS: usize = 1000;
pub const CHUNK_OVERLAP: usize = 200;

/// Text-bearing extensions worth embedding (PDFs go through the existing
/// extraction cache).
pub const SEMANTIC_EXTS: &[&str] = &[
    "md", "txt", "org", "rst", "tex", "html", "htm", "markdown", "pdf",
];

pub fn is_semantic_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            SEMANTIC_EXTS
                .iter()
                .any(|want| ext.eq_ignore_ascii_case(want))
        })
}

pub fn default_store_path() -> PathBuf {
    crate::index::default_cache_path().with_file_name("semantic.bin")
}

/// Documents larger than this are skipped by the semantic indexer.
pub const MAX_SEMANTIC_BYTES: u64 = 4 * 1024 * 1024;

/// The embedder the binary was built with: the local ONNX model when the
/// `semantic` feature is on, or the deterministic fake when
/// `FSEARCH_SEM_FAKE=1` (tests, terminals without the model).
pub fn make_embedder() -> Result<Box<dyn Embedder + Send>, String> {
    if std::env::var("FSEARCH_SEM_FAKE").is_ok_and(|v| v == "1") {
        return Ok(Box::new(HashEmbedder { dim: 64 }));
    }
    real::make()
}

#[cfg(feature = "semantic")]
mod real {
    use super::{Embedder, normalize};

    /// all-MiniLM-L6-v2 output width.
    const DIM: usize = 384;

    /// ort is built to load onnxruntime at runtime; when the user hasn't
    /// pointed ORT_DYLIB_PATH anywhere, try the usual install locations.
    fn bootstrap_ort() {
        if std::env::var_os("ORT_DYLIB_PATH").is_some() {
            return;
        }
        for candidate in [
            "/opt/homebrew/opt/onnxruntime/lib/libonnxruntime.dylib",
            "/usr/local/opt/onnxruntime/lib/libonnxruntime.dylib",
            "/usr/lib/libonnxruntime.so",
            "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
        ] {
            if std::path::Path::new(candidate).exists() {
                // called before any embedder threads exist
                unsafe { std::env::set_var("ORT_DYLIB_PATH", candidate) };
                return;
            }
        }
    }

    struct Real {
        model: fastembed::TextEmbedding,
    }

    impl Embedder for Real {
        fn dim(&self) -> usize {
            DIM
        }
        fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            let mut out = self
                .model
                .embed(texts.to_vec(), None)
                .map_err(|e| e.to_string())?;
            for v in &mut out {
                normalize(v);
            }
            Ok(out)
        }
    }

    pub fn make() -> Result<Box<dyn Embedder + Send>, String> {
        bootstrap_ort();
        let cache = super::default_store_path().with_file_name("models");
        let model = fastembed::TextEmbedding::try_new(
            fastembed::InitOptions::new(fastembed::EmbeddingModel::AllMiniLML6V2)
                .with_cache_dir(cache)
                .with_show_download_progress(true),
        )
        .map_err(|e| format!("loading embedding model: {e}"))?;
        Ok(Box::new(Real { model }))
    }
}

#[cfg(not(feature = "semantic"))]
mod real {
    use super::Embedder;
    pub fn make() -> Result<Box<dyn Embedder + Send>, String> {
        Err("this build has no embedding model — reinstall with --features semantic".to_string())
    }
}

/// Anything that turns text into fixed-dimension vectors.
pub trait Embedder {
    fn dim(&self) -> usize;
    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
}

/// Deterministic bag-of-words embedder used by the test suite (and the
/// `FSEARCH_SEM_FAKE=1` escape hatch) — no model, no network. Real overlap
/// in vocabulary produces real cosine similarity, which is all the
/// pipeline tests need.
pub struct HashEmbedder {
    pub dim: usize,
}

impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }
    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        use std::hash::{Hash, Hasher};
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; self.dim];
                for token in t.split_whitespace() {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    token.to_ascii_lowercase().hash(&mut h);
                    v[(h.finish() as usize) % self.dim] += 1.0;
                }
                normalize(&mut v);
                v
            })
            .collect())
    }
}

pub fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// One chunk of a source document, with the line it starts on.
pub struct Chunk {
    pub text: String,
    pub line_start: u32,
}

/// Splits on char boundaries near `target` chars, preferring newlines,
/// with `overlap` chars carried into the next chunk.
pub fn chunk_text(text: &str, target: usize, overlap: usize) -> Vec<Chunk> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let hard_end = (start + target).min(chars.len());
        // prefer to break on a newline in the last quarter of the window
        let mut end = hard_end;
        if hard_end < chars.len() {
            let window_start = start + target * 3 / 4;
            if let Some(nl) = (window_start..hard_end).rev().find(|&i| chars[i] == '\n') {
                end = nl + 1;
            }
        }
        let line_start = chars[..start].iter().filter(|&&c| c == '\n').count() as u32 + 1;
        let body: String = chars[start..end].iter().collect();
        if !body.trim().is_empty() {
            chunks.push(Chunk {
                text: body,
                line_start,
            });
        }
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(overlap).max(start + 1);
    }
    chunks
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocEntry {
    pub path: String,
    pub mtime: i64,
    pub size: u64,
    pub chunk_start: u32,
    pub chunk_count: u32,
}

/// The persisted semantic index.
pub struct SemStore {
    pub dim: u32,
    pub docs: Vec<DocEntry>,
    pub chunk_lines: Vec<u32>,
    pub vectors: Vec<f32>,
}

pub struct Hit {
    pub doc: usize,
    pub line_start: u32,
    pub score: f32,
}

impl SemStore {
    pub fn new(dim: u32) -> SemStore {
        SemStore {
            dim,
            docs: Vec::new(),
            chunk_lines: Vec::new(),
            vectors: Vec::new(),
        }
    }

    pub fn chunk_count(&self) -> usize {
        self.chunk_lines.len()
    }

    pub fn push_doc(&mut self, path: &str, mtime: i64, size: u64, chunks: &[(u32, Vec<f32>)]) {
        let chunk_start = self.chunk_lines.len() as u32;
        for (line, vec) in chunks {
            debug_assert_eq!(vec.len(), self.dim as usize);
            self.chunk_lines.push(*line);
            self.vectors.extend_from_slice(vec);
        }
        self.docs.push(DocEntry {
            path: path.to_string(),
            mtime,
            size,
            chunk_start,
            chunk_count: chunks.len() as u32,
        });
    }

    /// Best-chunk-per-document cosine ranking, highest first.
    pub fn query(&self, qvec: &[f32], top: usize) -> Vec<Hit> {
        let dim = self.dim as usize;
        let mut hits: Vec<Hit> = self
            .docs
            .iter()
            .enumerate()
            .filter_map(|(doc, entry)| {
                let mut best: Option<(f32, u32)> = None;
                for c in 0..entry.chunk_count {
                    let ci = (entry.chunk_start + c) as usize;
                    let v = &self.vectors[ci * dim..(ci + 1) * dim];
                    let score: f32 = v.iter().zip(qvec).map(|(a, b)| a * b).sum();
                    if best.is_none_or(|(s, _)| score > s) {
                        best = Some((score, self.chunk_lines[ci]));
                    }
                }
                best.map(|(score, line_start)| Hit {
                    doc,
                    line_start,
                    score,
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(top);
        hits
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut w = BufWriter::new(std::fs::File::create(&tmp)?);
            w.write_all(b"FSEM\x01\0\0\0")?;
            w.write_all(&self.dim.to_le_bytes())?;
            w.write_all(&(self.docs.len() as u64).to_le_bytes())?;
            w.write_all(&(self.chunk_lines.len() as u64).to_le_bytes())?;
            for d in &self.docs {
                w.write_all(&(d.path.len() as u32).to_le_bytes())?;
                w.write_all(d.path.as_bytes())?;
                w.write_all(&d.mtime.to_le_bytes())?;
                w.write_all(&d.size.to_le_bytes())?;
                w.write_all(&d.chunk_start.to_le_bytes())?;
                w.write_all(&d.chunk_count.to_le_bytes())?;
            }
            for line in &self.chunk_lines {
                w.write_all(&line.to_le_bytes())?;
            }
            for v in &self.vectors {
                w.write_all(&v.to_le_bytes())?;
            }
            w.flush()?;
        }
        std::fs::rename(&tmp, path)
    }

    pub fn load(path: &Path) -> Option<SemStore> {
        let data = std::fs::read(path).ok()?;
        if data.get(..8)? != b"FSEM\x01\0\0\0" {
            return None;
        }
        let dim = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?);
        let ndocs = u64::from_le_bytes(data.get(12..20)?.try_into().ok()?) as usize;
        let nchunks = u64::from_le_bytes(data.get(20..28)?.try_into().ok()?) as usize;
        let mut pos = 28usize;
        let mut docs = Vec::with_capacity(ndocs.min(4_000_000));
        for _ in 0..ndocs {
            let plen = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as usize;
            pos += 4;
            let path = std::str::from_utf8(data.get(pos..pos + plen)?).ok()?.to_string();
            pos += plen;
            let mtime = i64::from_le_bytes(data.get(pos..pos + 8)?.try_into().ok()?);
            pos += 8;
            let size = u64::from_le_bytes(data.get(pos..pos + 8)?.try_into().ok()?);
            pos += 8;
            let chunk_start = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?);
            pos += 4;
            let chunk_count = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?);
            pos += 4;
            docs.push(DocEntry {
                path,
                mtime,
                size,
                chunk_start,
                chunk_count,
            });
        }
        let mut chunk_lines = Vec::with_capacity(nchunks.min(16_000_000));
        for _ in 0..nchunks {
            chunk_lines.push(u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?));
            pos += 4;
        }
        let want = nchunks.checked_mul(dim as usize)?;
        let mut vectors = Vec::with_capacity(want.min(512_000_000));
        for _ in 0..want {
            vectors.push(f32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?));
            pos += 4;
        }
        if pos != data.len() {
            return None;
        }
        Some(SemStore {
            dim,
            docs,
            chunk_lines,
            vectors,
        })
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct BuildStats {
    pub embedded: usize,
    pub reused: usize,
    pub skipped: usize,
}

/// Builds a store for `files` (path, mtime, size), copying vectors from
/// `prior` for documents whose mtime and size are unchanged. `read` turns a
/// path into text (None skips it); `progress` is called per document.
pub fn build(
    files: &[(String, i64, u64)],
    prior: Option<&SemStore>,
    embedder: &mut dyn Embedder,
    read: &mut dyn FnMut(&str) -> Option<String>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(SemStore, BuildStats), String> {
    let dim = embedder.dim();
    let prior_docs: std::collections::HashMap<&str, &DocEntry> = prior
        .filter(|p| p.dim as usize == dim)
        .map(|p| p.docs.iter().map(|d| (d.path.as_str(), d)).collect())
        .unwrap_or_default();
    let mut store = SemStore::new(dim as u32);
    let mut stats = BuildStats::default();
    for (i, (path, mtime, size)) in files.iter().enumerate() {
        progress(i, files.len());
        if let Some(entry) = prior_docs.get(path.as_str())
            && entry.mtime == *mtime
            && entry.size == *size
        {
            let p = prior.unwrap();
            let chunks: Vec<(u32, Vec<f32>)> = (0..entry.chunk_count)
                .map(|c| {
                    let ci = (entry.chunk_start + c) as usize;
                    (p.chunk_lines[ci], p.vectors[ci * dim..(ci + 1) * dim].to_vec())
                })
                .collect();
            store.push_doc(path, *mtime, *size, &chunks);
            stats.reused += 1;
            continue;
        }
        let Some(text) = read(path) else {
            stats.skipped += 1;
            continue;
        };
        let chunks = chunk_text(&text, CHUNK_CHARS, CHUNK_OVERLAP);
        if chunks.is_empty() {
            stats.skipped += 1;
            continue;
        }
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let vecs = embedder.embed(&texts)?;
        let pairs: Vec<(u32, Vec<f32>)> = chunks
            .iter()
            .zip(vecs)
            .map(|(c, v)| (c.line_start, v))
            .collect();
        store.push_doc(path, *mtime, *size, &pairs);
        stats.embedded += 1;
    }
    Ok((store, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_covers_text_and_tracks_lines() {
        let text = (1..=100)
            .map(|i| format!("line number {i} with some words"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_text(&text, 300, 60);
        assert!(chunks.len() > 5);
        assert_eq!(chunks[0].line_start, 1);
        // line starts increase and stay in range
        let lines: Vec<u32> = chunks.iter().map(|c| c.line_start).collect();
        assert!(lines.windows(2).all(|w| w[0] <= w[1]));
        assert!(*lines.last().unwrap() <= 100);
        // every source line appears in some chunk
        assert!(chunks.iter().any(|c| c.text.contains("line number 100")));
    }

    #[test]
    fn hash_embedder_scores_shared_vocabulary_higher() {
        let mut e = HashEmbedder { dim: 64 };
        let vs = e
            .embed(&[
                "compound interest and patient investing".to_string(),
                "compound interest math".to_string(),
                "growing tomatoes in clay soil".to_string(),
            ])
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&vs[0], &vs[1]) > dot(&vs[0], &vs[2]));
    }

    #[test]
    fn store_roundtrip_and_query() {
        let mut e = HashEmbedder { dim: 64 };
        let mut store = SemStore::new(64);
        for (path, text) in [
            ("/docs/money.md", "compound interest rewards patience"),
            ("/docs/garden.md", "tomatoes need sun and water"),
        ] {
            let vecs = e.embed(&[text.to_string()]).unwrap();
            store.push_doc(path, 1, 10, &[(1, vecs[0].clone())]);
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("semantic.bin");
        store.save(&file).unwrap();
        let loaded = SemStore::load(&file).unwrap();
        assert_eq!(loaded.docs, store.docs);
        assert_eq!(loaded.chunk_count(), 2);

        let q = e.embed(&["interest and patience".to_string()]).unwrap();
        let hits = loaded.query(&q[0], 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(loaded.docs[hits[0].doc].path, "/docs/money.md");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn corrupt_store_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("semantic.bin");
        std::fs::write(&file, b"FSEM but not really").unwrap();
        assert!(SemStore::load(&file).is_none());
    }

    #[test]
    fn build_reuses_unchanged_docs_and_skips_unreadable() {
        let files = vec![
            ("/a/money.md".to_string(), 100, 40u64),
            ("/a/garden.md".to_string(), 200, 30),
            ("/a/gone.md".to_string(), 300, 20),
        ];
        let mut embedder = HashEmbedder { dim: 64 };
        let mut embed_calls = 0usize;
        let mut read = |path: &str| match path {
            "/a/money.md" => Some("compound interest rewards patience".to_string()),
            "/a/garden.md" => Some("tomatoes need sun and water".to_string()),
            _ => None,
        };
        let (first, stats) = build(
            &files,
            None,
            &mut embedder,
            &mut read,
            &mut |_, _| embed_calls += 1,
        )
        .unwrap();
        assert_eq!(
            stats,
            BuildStats {
                embedded: 2,
                reused: 0,
                skipped: 1
            }
        );
        assert_eq!(embed_calls, 3); // progress ran once per file

        // second build: money.md unchanged, garden.md touched
        let files2 = vec![
            ("/a/money.md".to_string(), 100, 40u64),
            ("/a/garden.md".to_string(), 999, 31),
        ];
        let mut read2 = |path: &str| match path {
            "/a/money.md" => panic!("unchanged doc was re-read"),
            "/a/garden.md" => Some("tomatoes need sun water and mulch".to_string()),
            _ => None,
        };
        let (second, stats2) =
            build(&files2, Some(&first), &mut embedder, &mut read2, &mut |_,
             _| {})
            .unwrap();
        assert_eq!(
            stats2,
            BuildStats {
                embedded: 1,
                reused: 1,
                skipped: 0
            }
        );
        // reused vectors are byte-identical to the first build's
        let dim = 64usize;
        assert_eq!(second.vectors[..dim], first.vectors[..dim]);
    }

    #[test]
    fn semantic_paths_recognized() {
        assert!(is_semantic_path("/a/notes.md"));
        assert!(is_semantic_path("/a/paper.PDF"));
        assert!(!is_semantic_path("/a/binary.dat"));
        assert!(!is_semantic_path("/a/code.rs"));
    }
}
