use fsearch::sem::SemStore;
use fsearch::walker::FileMeta;
use std::path::Path;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, upper: usize) -> usize {
        if upper == 0 {
            0
        } else {
            self.next() as usize % upper
        }
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn mutate(valid: &[u8], round: usize, rng: &mut Rng) -> Vec<u8> {
    match round % 8 {
        0 => valid[..rng.below(valid.len() + 1)].to_vec(),
        1 => {
            let mut bytes = valid.to_vec();
            for _ in 0..=rng.below(8) {
                if !bytes.is_empty() {
                    let at = rng.below(bytes.len());
                    bytes[at] ^= 1 << rng.below(8);
                }
            }
            bytes
        }
        2 => {
            let mut bytes = valid.to_vec();
            if !bytes.is_empty() {
                let at = rng.below(bytes.len());
                let len = rng.below((bytes.len() - at).min(16) + 1);
                for byte in &mut bytes[at..at + len] {
                    *byte = rng.next() as u8;
                }
            }
            bytes
        }
        3 => {
            let mut bytes = valid.to_vec();
            let len = rng.below(65);
            bytes.extend(rng.bytes(len));
            bytes
        }
        4 => {
            let len = rng.below(513);
            rng.bytes(len)
        }
        5 => {
            let mut bytes = valid.to_vec();
            if !bytes.is_empty() {
                let start = rng.below(bytes.len());
                let end = start + rng.below(bytes.len() - start + 1);
                bytes.drain(start..end);
            }
            bytes
        }
        6 => {
            let mut bytes = valid.to_vec();
            let at = rng.below(bytes.len() + 1);
            let len = rng.below(33);
            bytes.splice(at..at, rng.bytes(len));
            bytes
        }
        _ => {
            let mut bytes = valid.to_vec();
            let start = rng.below(bytes.len().saturating_sub(7).max(1));
            for byte in bytes.iter_mut().skip(start).take(8) {
                *byte = 0xff;
            }
            bytes
        }
    }
}

fn exercise(valid: &[u8], file: &Path, mut load: impl FnMut(&Path)) {
    let mut rng = Rng(0x6a09_e667_f3bc_c909);
    for round in 0..1024 {
        std::fs::write(file, mutate(valid, round, &mut rng)).unwrap();
        load(file);
    }
}

#[test]
fn path_index_loader_rejects_mutated_bytes_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let valid_path = dir.path().join("valid-index.bin");
    fsearch::index::save(
        &[
            (
                "/docs/alpha.txt".to_string(),
                FileMeta { mtime: 7, size: 11 },
            ),
            ("/docs/βeta.md".to_string(), FileMeta { mtime: 5, size: 13 }),
        ],
        &valid_path,
    )
    .unwrap();
    let valid = std::fs::read(valid_path).unwrap();
    exercise(&valid, &dir.path().join("mutated-index.bin"), |path| {
        if let Some(store) = fsearch::index::load(path) {
            for i in 0..store.len() {
                let _ = (store.get(i), store.meta(i));
            }
        }
    });
}

#[test]
fn semantic_loader_rejects_mutated_bytes_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let valid_path = dir.path().join("valid-semantic.bin");
    let mut store = SemStore::new(4);
    store.push_doc("/docs/alpha.txt", 7, 11, &[(1, vec![0.5, 0.5, 0.5, 0.5])]);
    store.save(&valid_path).unwrap();
    let valid = std::fs::read(valid_path).unwrap();
    exercise(&valid, &dir.path().join("mutated-semantic.bin"), |path| {
        if let Some(store) = SemStore::load(path) {
            let query = vec![0.0; store.dim as usize];
            let _ = store.query(&query, 8);
        }
    });
}
