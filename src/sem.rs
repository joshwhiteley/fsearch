//! Semantic search: find documents by meaning (`? essay about patience`).
//! Text is chunked and embedded locally; queries run brute-force cosine
//! over every chunk — at home-directory scale (≤ ~1M chunks) that's a few
//! milliseconds, so there is no vector database and no daemon, in keeping
//! with the rest of fsearch.

use half::f16;
use memmap2::{Mmap, MmapOptions};
use rayon::prelude::*;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SAVE_COUNTER: AtomicU64 = AtomicU64::new(0);

const STORE_MAGIC: &[u8; 8] = b"FSEM\x02\0\0\0";
const HEADER_BYTES: usize = 28;
const MAX_DOCS: usize = 4_000_000;
const MAX_CHUNKS: usize = 16_000_000;
const MAX_DIM: u32 = 16_384;
const MAX_PATH_BYTES: usize = 1024 * 1024;

pub const CHUNK_CHARS: usize = 1000;
pub const CHUNK_OVERLAP: usize = 200;

/// Text-bearing extensions worth embedding (PDFs and Office files go through
/// their extraction caches).
pub const SEMANTIC_EXTS: &[&str] = &[
    "md", "txt", "org", "rst", "tex", "html", "htm", "markdown", "pdf", "docx", "xlsx",
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
            let mut out = self.model.embed(texts, None).map_err(|e| e.to_string())?;
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
    if target == 0 {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let hard_end = start.saturating_add(target).min(chars.len());
        // prefer to break on a newline in the last quarter of the window
        let mut end = hard_end;
        if hard_end < chars.len() {
            let window_start = start
                .saturating_add(target.saturating_mul(3) / 4)
                .min(hard_end);
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

enum VectorStorage {
    Owned(Vec<u16>),
    Mapped {
        mmap: Mmap,
        vector_offset: usize,
        vector_count: usize,
    },
}

impl VectorStorage {
    fn len(&self) -> usize {
        match self {
            Self::Owned(bits) => bits.len(),
            Self::Mapped { vector_count, .. } => *vector_count,
        }
    }

    fn bits_at(&self, index: usize) -> Option<u16> {
        match self {
            Self::Owned(bits) => bits.get(index).copied(),
            Self::Mapped {
                mmap,
                vector_offset,
                vector_count,
            } if index < *vector_count => {
                let offset = vector_offset.checked_add(index.checked_mul(2)?)?;
                let bytes = mmap.get(offset..offset.checked_add(2)?)?;
                Some(u16::from_le_bytes([bytes[0], bytes[1]]))
            }
            _ => None,
        }
    }

    fn write_le<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        match self {
            Self::Owned(bits) => {
                for bits in bits {
                    writer.write_all(&bits.to_le_bytes())?;
                }
            }
            Self::Mapped {
                mmap,
                vector_offset,
                vector_count,
            } => {
                let bytes = vector_count
                    .checked_mul(2)
                    .and_then(|len| vector_offset.checked_add(len))
                    .and_then(|end| mmap.get(*vector_offset..end))
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid mapped semantic vector range",
                        )
                    })?;
                writer.write_all(bytes)?;
            }
        }
        Ok(())
    }
}

/// The persisted semantic index. Builders own f16 bits; loaded stores retain a
/// read-only mapping of the vector tail instead of allocating another copy.
pub struct SemStore {
    pub dim: u32,
    pub docs: Vec<DocEntry>,
    pub chunk_lines: Vec<u32>,
    vectors: VectorStorage,
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
            vectors: VectorStorage::Owned(Vec::new()),
        }
    }

    pub fn chunk_count(&self) -> usize {
        self.chunk_lines.len()
    }

    pub fn push_doc(&mut self, path: &str, mtime: i64, size: u64, chunks: &[(u32, Vec<f32>)]) {
        let mut quantized = Vec::with_capacity(chunks.len());
        for (line, vector) in chunks {
            if vector.len() != self.dim as usize {
                continue;
            }
            quantized.push((
                *line,
                vector.iter().map(|x| f16::from_f32(*x).to_bits()).collect(),
            ));
        }
        self.push_doc_bits(path, mtime, size, &quantized);
    }

    fn push_doc_bits(&mut self, path: &str, mtime: i64, size: u64, chunks: &[(u32, Vec<u16>)]) {
        let chunk_start = self.chunk_lines.len() as u32;
        for (line, _) in chunks {
            self.chunk_lines.push(*line);
        }
        let vectors = self.owned_vectors();
        for (_, bits) in chunks {
            vectors.extend_from_slice(bits);
        }
        self.docs.push(DocEntry {
            path: path.to_string(),
            mtime,
            size,
            chunk_start,
            chunk_count: chunks.len() as u32,
        });
    }

    fn copy_doc_from(&mut self, prior: &Self, entry: &DocEntry) {
        let chunk_start = self.chunk_lines.len() as u32;
        let dim = self.dim as usize;
        for c in 0..entry.chunk_count as usize {
            let source_chunk = entry.chunk_start as usize + c;
            self.chunk_lines.push(prior.chunk_lines[source_chunk]);
        }
        let vectors = self.owned_vectors();
        for c in 0..entry.chunk_count as usize {
            let source_chunk = entry.chunk_start as usize + c;
            let start = source_chunk * dim;
            for component in 0..dim {
                vectors.push(prior.vectors.bits_at(start + component).unwrap());
            }
        }
        self.docs.push(DocEntry {
            path: entry.path.clone(),
            mtime: entry.mtime,
            size: entry.size,
            chunk_start,
            chunk_count: entry.chunk_count,
        });
    }

    fn owned_vectors(&mut self) -> &mut Vec<u16> {
        if let VectorStorage::Mapped { vector_count, .. } = &self.vectors {
            let mut bits = Vec::with_capacity(*vector_count);
            for i in 0..*vector_count {
                if let Some(value) = self.vectors.bits_at(i) {
                    bits.push(value);
                } else {
                    break;
                }
            }
            self.vectors = VectorStorage::Owned(bits);
        }
        match &mut self.vectors {
            VectorStorage::Owned(bits) => bits,
            VectorStorage::Mapped { .. } => unreachable!(),
        }
    }

    fn vector_bits_at(&self, index: usize) -> Option<u16> {
        self.vectors.bits_at(index)
    }

    /// Best-chunk-per-document cosine ranking, highest first. Each
    /// document's chunk loop stays scalar; documents are scored in parallel.
    pub fn query(&self, qvec: &[f32], top: usize) -> Vec<Hit> {
        let dim = self.dim as usize;
        if dim == 0 || qvec.len() != dim {
            return Vec::new();
        }
        let mut hits: Vec<Hit> = self
            .docs
            .par_iter()
            .enumerate()
            .filter_map(|(doc, entry)| {
                let end = entry.chunk_start.checked_add(entry.chunk_count)? as usize;
                let start = entry.chunk_start as usize;
                if end > self.chunk_lines.len() || end.checked_mul(dim)? > self.vectors.len() {
                    return None;
                }
                let mut best: Option<(f32, u32)> = None;
                for ci in start..end {
                    let base = ci.checked_mul(dim)?;
                    let mut score = 0.0;
                    for (component, query) in qvec.iter().enumerate() {
                        let value = f16::from_bits(self.vector_bits_at(base + component)?).to_f32();
                        score += value * query;
                    }
                    if score.is_finite() && best.is_none_or(|(s, _)| score > s) {
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
        self.validate_for_save()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let nonce = SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
        {
            let mut w = BufWriter::new(std::fs::File::create(&tmp)?);
            w.write_all(STORE_MAGIC)?;
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
            self.vectors.write_le(&mut w)?;
            w.flush()?;
        }
        std::fs::rename(&tmp, path)
    }

    fn validate_for_save(&self) -> std::io::Result<()> {
        if self.dim == 0 || self.dim > MAX_DIM {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid semantic vector dimension",
            ));
        }
        if self.docs.len() > MAX_DOCS
            || self.chunk_lines.len() > MAX_CHUNKS
            || self.chunk_lines.len() > u32::MAX as usize
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "semantic store is too large",
            ));
        }
        let vector_count = self
            .chunk_lines
            .len()
            .checked_mul(self.dim as usize)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "semantic vector count overflows",
                )
            })?;
        if self.vectors.len() != vector_count {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "semantic vector storage does not match chunks",
            ));
        }
        let mut expected_chunk = 0u32;
        for doc in &self.docs {
            if doc.path.len() > MAX_PATH_BYTES
                || doc.path.len() > u32::MAX as usize
                || doc.chunk_start != expected_chunk
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid semantic document range",
                ));
            }
            expected_chunk = doc
                .chunk_start
                .checked_add(doc.chunk_count)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "semantic document range overflows",
                    )
                })?;
        }
        if expected_chunk as usize != self.chunk_lines.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "semantic documents do not cover chunks",
            ));
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Option<SemStore> {
        let file = std::fs::File::open(path).ok()?;
        // SAFETY: this is a read-only mapping owned by the returned store. The
        // writer publishes new stores with rename, so it never overwrites this
        // mapped inode in place.
        let mmap = unsafe { MmapOptions::new().map(&file).ok()? };
        Self::from_mmap(mmap)
    }

    fn from_mmap(mmap: Mmap) -> Option<SemStore> {
        let data = &mmap[..];
        if data.len() < HEADER_BYTES || data.get(..8)? != STORE_MAGIC {
            return None;
        }
        let dim = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?);
        if dim == 0 || dim > MAX_DIM {
            return None;
        }
        let ndocs = usize::try_from(u64::from_le_bytes(data.get(12..20)?.try_into().ok()?)).ok()?;
        let nchunks =
            usize::try_from(u64::from_le_bytes(data.get(20..28)?.try_into().ok()?)).ok()?;
        if ndocs > MAX_DOCS || nchunks > MAX_CHUNKS || nchunks > u32::MAX as usize {
            return None;
        }
        let lines_bytes = nchunks.checked_mul(4)?;
        let vector_count = nchunks.checked_mul(dim as usize)?;
        let vectors_bytes = vector_count.checked_mul(2)?;
        let tail_bytes = lines_bytes.checked_add(vectors_bytes)?;
        let docs_end = data.len().checked_sub(tail_bytes)?;
        if docs_end < HEADER_BYTES {
            return None;
        }

        let min_docs_bytes = ndocs.checked_mul(28)?;
        if HEADER_BYTES.checked_add(min_docs_bytes)? > docs_end {
            return None;
        }
        let mut docs_reader = ByteReader::new(data.get(HEADER_BYTES..docs_end)?);
        let mut docs = Vec::new();
        docs.try_reserve_exact(ndocs).ok()?;
        let mut expected_chunk = 0u32;
        for _ in 0..ndocs {
            let path_len = usize::try_from(docs_reader.u32()?).ok()?;
            if path_len > MAX_PATH_BYTES {
                return None;
            }
            let path_text = std::str::from_utf8(docs_reader.take(path_len)?).ok()?;
            let mut path = String::new();
            path.try_reserve(path_len).ok()?;
            path.push_str(path_text);
            let mtime = i64::from_le_bytes(docs_reader.take(8)?.try_into().ok()?);
            let size = u64::from_le_bytes(docs_reader.take(8)?.try_into().ok()?);
            let chunk_start = docs_reader.u32()?;
            let chunk_count = docs_reader.u32()?;
            if chunk_start != expected_chunk {
                return None;
            }
            expected_chunk = chunk_start.checked_add(chunk_count)?;
            docs.push(DocEntry {
                path,
                mtime,
                size,
                chunk_start,
                chunk_count,
            });
        }
        if docs_reader.remaining() != 0 || expected_chunk as usize != nchunks {
            return None;
        }

        let vector_offset = docs_end.checked_add(lines_bytes)?;
        if vector_offset.checked_add(vectors_bytes)? != data.len() {
            return None;
        }
        let mut lines_reader = ByteReader::new(data.get(docs_end..vector_offset)?);
        let mut chunk_lines = Vec::new();
        chunk_lines.try_reserve_exact(nchunks).ok()?;
        for _ in 0..nchunks {
            chunk_lines.push(lines_reader.u32()?);
        }
        if lines_reader.remaining() != 0 {
            return None;
        }
        Some(SemStore {
            dim,
            docs,
            chunk_lines,
            vectors: VectorStorage::Mapped {
                mmap,
                vector_offset,
                vector_count,
            },
        })
    }
}

struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let bytes = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
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
    // accumulate docs that need embedding and flush them in batches so a
    // many-file walk makes one embedder call per ~64 chunks, not one per file
    let mut pending: Vec<(&str, i64, u64, Vec<Chunk>)> = Vec::new();
    let mut pending_chunks = 0usize;
    fn flush_batch(
        store: &mut SemStore,
        embedder: &mut dyn Embedder,
        stats: &mut BuildStats,
        pending: &mut Vec<(&str, i64, u64, Vec<Chunk>)>,
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let all_texts: Vec<String> = pending
            .iter()
            .flat_map(|(_, _, _, chunks)| chunks.iter().map(|c| c.text.clone()))
            .collect();
        let all_vecs = embedder.embed(&all_texts)?;
        if all_vecs.len() != all_texts.len()
            || all_vecs
                .iter()
                .any(|vector| vector.len() != store.dim as usize)
        {
            return Err("embedder returned vectors with the wrong shape".to_string());
        }
        let mut vecs = all_vecs.into_iter();
        for (path, mtime, size, chunks) in pending.drain(..) {
            let pairs: Vec<(u32, Vec<f32>)> = chunks
                .iter()
                .zip(&mut vecs)
                .map(|(c, v)| (c.line_start, v.clone()))
                .collect();
            stats.embedded += 1;
            store.push_doc(path, mtime, size, &pairs);
        }
        Ok(())
    }
    for (i, (path, mtime, size)) in files.iter().enumerate() {
        progress(i, files.len());
        if let Some(entry) = prior_docs.get(path.as_str())
            && entry.mtime == *mtime
            && entry.size == *size
        {
            let p = prior.unwrap();
            store.copy_doc_from(p, entry);
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
        pending_chunks += chunks.len();
        pending.push((path.as_str(), *mtime, *size, chunks));
        if pending_chunks >= 64 {
            flush_batch(&mut store, embedder, &mut stats, &mut pending)?;
            pending_chunks = 0;
        }
    }
    flush_batch(&mut store, embedder, &mut stats, &mut pending)?;
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
    fn chunking_preserves_characters_across_boundaries() {
        for len in [0, 1, 2, 11, 37, 129] {
            let mut text = String::new();
            for i in 0..len {
                text.push(char::from_u32(0x1000 + i as u32).unwrap());
                if i % 11 == 10 {
                    text.push('\n');
                }
            }
            for target in [1, 2, 7, 16, 64, usize::MAX] {
                for overlap in [0, 1, target / 2, target.saturating_sub(1), target] {
                    let chunks = chunk_text(&text, target, overlap);
                    let mut seen = vec![false; len];
                    let mut prior_line = 0;
                    for chunk in chunks {
                        assert!(!chunk.text.trim().is_empty());
                        assert!(chunk.line_start >= prior_line);
                        prior_line = chunk.line_start;
                        for ch in chunk.text.chars().filter(|ch| *ch != '\n') {
                            let index = (ch as u32 - 0x1000) as usize;
                            seen[index] = true;
                        }
                    }
                    assert!(seen.into_iter().all(|value| value));
                }
            }
        }
        assert!(chunk_text("text", 0, 10).is_empty());
        assert!(chunk_text(" \n\t", 2, 1).is_empty());
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
    fn f16_precision_and_mmap_loading() {
        let mut store = SemStore::new(3);
        let vector = vec![1.0, 0.3333, -2.5];
        store.push_doc("/a/doc.md", 1, 10, &[(7, vector.clone())]);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("semantic.bin");
        store.save(&file).unwrap();
        let loaded = SemStore::load(&file).unwrap();
        assert!(matches!(loaded.vectors, VectorStorage::Mapped { .. }));
        for (i, value) in vector.iter().enumerate() {
            assert_eq!(
                loaded.vector_bits_at(i),
                Some(f16::from_f32(*value).to_bits())
            );
        }
        let expected: f32 = vector
            .iter()
            .map(|value| f16::from_f32(*value).to_f32() * value)
            .sum();
        let score = loaded.query(&vector, 1)[0].score;
        assert!(
            (score - expected).abs() < 0.002,
            "score={score} expected={expected}"
        );
        let copied = dir.path().join("copied.bin");
        loaded.save(&copied).unwrap();
        assert!(matches!(
            SemStore::load(&copied).unwrap().vectors,
            VectorStorage::Mapped { .. }
        ));

        // Atomic replacement of the path does not invalidate the old mapping.
        let replacement = dir.path().join("replacement.bin");
        std::fs::write(&replacement, b"not the mapped store").unwrap();
        std::fs::rename(replacement, &file).unwrap();
        assert_eq!(loaded.query(&vector, 1)[0].line_start, 7);
    }

    #[test]
    fn corrupt_store_lengths_ranges_and_old_version_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("semantic.bin");
        for len in 0..HEADER_BYTES {
            std::fs::write(&file, vec![0; len]).unwrap();
            assert!(SemStore::load(&file).is_none(), "accepted {len}-byte file");
        }

        let mut huge = Vec::from(*STORE_MAGIC);
        huge.extend_from_slice(&64u32.to_le_bytes());
        huge.extend_from_slice(&u64::MAX.to_le_bytes());
        huge.extend_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&file, huge).unwrap();
        assert!(std::panic::catch_unwind(|| SemStore::load(&file)).is_ok());
        assert!(SemStore::load(&file).is_none());

        let mut bounded_counts = Vec::from(*STORE_MAGIC);
        bounded_counts.extend_from_slice(&64u32.to_le_bytes());
        bounded_counts.extend_from_slice(&(MAX_DOCS as u64).to_le_bytes());
        bounded_counts.extend_from_slice(&0u64.to_le_bytes());
        std::fs::write(&file, bounded_counts).unwrap();
        assert!(SemStore::load(&file).is_none());

        let mut store = SemStore::new(2);
        store.push_doc("/a", 1, 2, &[(1, vec![1.0, 0.0])]);
        store.save(&file).unwrap();
        let mut bytes = std::fs::read(&file).unwrap();
        // v1 must rebuild rather than be interpreted as f16 data.
        bytes[4] = 1;
        std::fs::write(&file, &bytes).unwrap();
        assert!(SemStore::load(&file).is_none());
        store.save(&file).unwrap();
        let mut bytes = std::fs::read(&file).unwrap();
        // The first document's chunk_start follows path, mtime, and size.
        let chunk_start = 28 + 4 + 2 + 8 + 8;
        bytes[chunk_start..chunk_start + 4].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(&file, &bytes).unwrap();
        assert!(SemStore::load(&file).is_none());
        store.save(&file).unwrap();
        let mut bytes = std::fs::read(&file).unwrap();
        bytes.push(0);
        std::fs::write(&file, &bytes).unwrap();
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
        let (first, stats) = build(&files, None, &mut embedder, &mut read, &mut |_, _| {
            embed_calls += 1
        })
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

        // A loaded mmap store is also reusable without expanding vectors back
        // to f32 or rereading unchanged documents.
        let mmap_dir = tempfile::tempdir().unwrap();
        let mmap_file = mmap_dir.path().join("semantic.bin");
        first.save(&mmap_file).unwrap();
        let mapped = SemStore::load(&mmap_file).unwrap();
        let unchanged = vec![
            ("/a/money.md".to_string(), 100, 40u64),
            ("/a/garden.md".to_string(), 200, 30u64),
        ];
        let mut no_read =
            |_: &str| -> Option<String> { panic!("unchanged mmap document was re-read") };
        let (reused, mapped_stats) = build(
            &unchanged,
            Some(&mapped),
            &mut embedder,
            &mut no_read,
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(mapped_stats.reused, 2);
        assert_eq!(reused.vector_bits_at(0), first.vector_bits_at(0));

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
        let (second, stats2) = build(
            &files2,
            Some(&first),
            &mut embedder,
            &mut read2,
            &mut |_, _| {},
        )
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
        for i in 0..dim {
            assert_eq!(second.vector_bits_at(i), first.vector_bits_at(i));
        }
    }

    #[test]
    fn semantic_paths_recognized() {
        assert!(is_semantic_path("/a/notes.md"));
        assert!(is_semantic_path("/a/paper.PDF"));
        assert!(is_semantic_path("/a/report.DOCX"));
        assert!(is_semantic_path("/a/budget.xlsx"));
        assert!(!is_semantic_path("/a/binary.dat"));
        assert!(!is_semantic_path("/a/code.rs"));
    }
}
