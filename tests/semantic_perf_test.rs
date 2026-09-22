//! Synthetic scalar-ranking oracle and opt-in semantic query benchmark.
//! No model, network, or user documents are involved.
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use fsearch::sem::{DocEntry, Hit, SemStore, normalize};
use half::f16;

struct Fixture {
    dim: usize,
    docs: Vec<Vec<(u32, Vec<f32>)>>,
}

impl Fixture {
    fn generated(dim: usize, docs: usize, chunks: usize) -> Self {
        let mut state = 0x7812_34ab_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
        };
        let docs = (0..docs)
            .map(|_| {
                (0..chunks)
                    .map(|chunk| {
                        let mut vector: Vec<f32> = (0..dim).map(|_| next()).collect();
                        normalize(&mut vector);
                        (chunk as u32 * 11 + 1, vector)
                    })
                    .collect()
            })
            .collect();
        Self { dim, docs }
    }

    fn owned(&self) -> SemStore {
        let mut store = SemStore::new(self.dim as u32);
        for (i, chunks) in self.docs.iter().enumerate() {
            store.push_doc(&format!("/synthetic/{i}.txt"), i as i64, i as u64, chunks);
        }
        store
    }

    fn legacy(&self, path: &Path) -> SemStore {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FSEM\x01\0\0\0");
        bytes.extend_from_slice(&(self.dim as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.docs.len() as u64).to_le_bytes());
        let chunks: usize = self.docs.iter().map(Vec::len).sum();
        bytes.extend_from_slice(&(chunks as u64).to_le_bytes());
        let mut start = 0u32;
        for (i, chunks) in self.docs.iter().enumerate() {
            let name = format!("/synthetic/{i}.txt");
            bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(&(i as i64).to_le_bytes());
            bytes.extend_from_slice(&(i as u64).to_le_bytes());
            bytes.extend_from_slice(&start.to_le_bytes());
            bytes.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
            start += chunks.len() as u32;
        }
        for chunks in &self.docs {
            for (line, _) in chunks {
                bytes.extend_from_slice(&line.to_le_bytes());
            }
        }
        for chunks in &self.docs {
            for (_, vector) in chunks {
                for value in vector {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        std::fs::write(path, bytes).unwrap();
        let store = SemStore::load(path).unwrap();
        assert!(store.needs_migration());
        store
    }

    // Independent serial baseline: source fixture vectors, not production
    // storage accessors or rank helpers. Compare score bits, not tolerances.
    fn oracle(&self, query: &[f32], top: usize, filter: Filter, legacy: bool) -> Vec<Hit> {
        if top == 0 || query.len() != self.dim || self.dim == 0 {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for (doc, chunks) in self.docs.iter().enumerate() {
            if !filter.accepts(doc as u64) {
                continue;
            }
            let mut best: Option<Hit> = None;
            for (line, vector) in chunks {
                let mut score = 0.0f32;
                for i in 0..self.dim {
                    let value = if legacy {
                        vector[i]
                    } else {
                        f16::from_f32(vector[i]).to_f32()
                    };
                    score += value * query[i];
                }
                if !score.is_finite() {
                    continue;
                }
                if best.as_ref().is_none_or(|hit| score > hit.score) {
                    best = Some(Hit {
                        doc,
                        line_start: *line,
                        score,
                    });
                }
            }
            if let Some(hit) = best {
                hits.push(hit);
            }
        }
        hits.sort_by(|a, b| {
            if a.score.to_bits() == b.score.to_bits() {
                a.doc.cmp(&b.doc)
            } else {
                b.score.total_cmp(&a.score)
            }
        });
        hits.truncate(top);
        hits
    }
}

#[derive(Clone, Copy, Debug)]
enum Filter {
    All,
    Sparse,
    None,
}

impl Filter {
    fn accepts(self, id: u64) -> bool {
        match self {
            Self::All => true,
            Self::Sparse => id.is_multiple_of(97),
            Self::None => false,
        }
    }

    fn matches(self, doc: &DocEntry) -> bool {
        self.accepts(doc.size)
    }
}

fn assert_hits(actual: &[Hit], expected: &[Hit]) {
    let repr = |hits: &[Hit]| {
        hits.iter()
            .map(|h| (h.doc, h.line_start, h.score.to_bits()))
            .collect::<Vec<_>>()
    };
    assert_eq!(repr(actual), repr(expected));
}

#[test]
fn scalar_oracle_matches_owned_mmap_and_legacy_queries() {
    for dim in [1, 3, 17, 64, 384] {
        let mut fixture = Fixture::generated(dim, 211, 3);
        // Multiple tied chunks/documents must pick the first chunk and doc id.
        fixture.docs[0] = vec![(91, vec![0.0; dim]), (2, vec![0.0; dim])];
        fixture.docs[1] = fixture.docs[0].clone();
        fixture.docs[2].clear();
        fixture.docs[3][0].1[0] = f32::NAN;
        fixture.docs[4][0].1[0] = f32::INFINITY;
        fixture.docs[5] = vec![(8, vec![f32::NEG_INFINITY; dim])];
        fixture.docs[6][0].1[0] = f32::MAX; // overflows only in f16
        fixture.docs[7][0].1[0] = f32::from_bits(1); // subnormal/quantized zero
        let dir = tempfile::tempdir().unwrap();
        let owned = fixture.owned();
        let path = dir.path().join("v2.bin");
        owned.save(&path).unwrap();
        let mapped = SemStore::load(&path).unwrap();
        let legacy = fixture.legacy(&dir.path().join("v1.bin"));
        let mut query = (0..dim).map(|i| (i as f32 + 0.7).sin()).collect::<Vec<_>>();
        normalize(&mut query);
        for query in [
            query,
            vec![0.0; dim],
            vec![-0.0; dim],
            vec![f32::NAN; dim],
            vec![1.0; dim + 1],
        ] {
            for filter in [Filter::All, Filter::Sparse, Filter::None] {
                for top in [0, 1, 7, 100, fixture.docs.len(), usize::MAX] {
                    let expected = fixture.oracle(&query, top, filter, false);
                    assert_hits(
                        &owned.query_filtered(&query, top, |d| filter.matches(d)),
                        &expected,
                    );
                    assert_hits(
                        &mapped.query_filtered(&query, top, |d| filter.matches(d)),
                        &expected,
                    );
                    assert_hits(
                        &legacy.query_filtered(&query, top, |d| filter.matches(d)),
                        &fixture.oracle(&query, top, filter, true),
                    );
                }
            }
        }
        assert_eq!(owned.query(&vec![0.0; dim], 1)[0].line_start, 91);
        // Migration still uses precisely the same f16 quantization as owned.
        let migrated_path = dir.path().join("migrated.bin");
        legacy.save(&migrated_path).unwrap();
        let migrated = SemStore::load(&migrated_path).unwrap();
        assert!(!migrated.needs_migration());
        assert_hits(
            &migrated.query(&vec![1.0; dim], 20),
            &owned.query(&vec![1.0; dim], 20),
        );
    }
}

#[test]
fn mutated_public_ranges_and_dimensions_stay_checked() {
    let fixture = Fixture::generated(3, 4, 1);
    let dir = tempfile::tempdir().unwrap();
    let owned = fixture.owned();
    let path = dir.path().join("v2.bin");
    owned.save(&path).unwrap();
    let mapped = SemStore::load(&path).unwrap();
    let legacy = fixture.legacy(&dir.path().join("v1.bin"));
    for mut store in [owned, mapped, legacy] {
        store.docs[0].chunk_start = u32::MAX;
        store.docs[0].chunk_count = 1;
        store.docs[1].chunk_count = u32::MAX;
        store.docs[2].chunk_start = 200;
        let hits = store.query(&[1.0, 0.0, 0.0], 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc, 3);
        store.dim = u32::MAX;
        assert!(store.query(&[1.0, 0.0, 0.0], 100).is_empty());
        store.dim = 0;
        assert!(store.query(&[], 100).is_empty());
    }
    let store = fixture.owned();
    assert!(
        store
            .query_filtered(&[1.0; 3], 0, |_| panic!("top zero called predicate"))
            .is_empty()
    );
}

fn median(mut operation: impl FnMut(), iterations: usize) -> Duration {
    for _ in 0..3 {
        operation();
    }
    let mut samples = Vec::new();
    for _ in 0..11 {
        let start = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        samples.push(start.elapsed() / iterations as u32);
    }
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "synthetic timing benchmark; run alone with --release --ignored --nocapture"]
fn synthetic_semantic_query_scaling() {
    eprintln!("rayon threads: {}", rayon::current_num_threads());
    for (dim, docs, chunks) in [(64, 32_768, 2), (384, 16_384, 4)] {
        let fixture = Fixture::generated(dim, docs, chunks);
        let owned = fixture.owned();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.bin");
        owned.save(&path).unwrap();
        let mapped = SemStore::load(&path).unwrap();
        let legacy = fixture.legacy(&dir.path().join("v1.bin"));
        let mut query = (0..dim).map(|i| (i as f32 + 0.7).sin()).collect::<Vec<_>>();
        normalize(&mut query);
        for (name, store) in [
            ("owned", owned),
            ("mmap-f16", mapped),
            ("legacy-f32", legacy),
        ] {
            for (filter, top) in [
                (Filter::All, 20),
                (Filter::Sparse, 20),
                (Filter::All, docs),
                (Filter::All, 0),
            ] {
                let elapsed = median(
                    || {
                        black_box(
                            store.query_filtered(black_box(&query), black_box(top), |d| {
                                filter.matches(d)
                            }),
                        );
                    },
                    if top == 0 { 10_000 } else { 3 },
                );
                eprintln!(
                    "dim={dim} docs={docs} chunks={chunks} store={name} filter={filter:?} top={top}: {elapsed:?}"
                );
            }
        }
    }
}
