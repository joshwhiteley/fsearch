use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Tracks which files get opened, to boost them in ranking. Backed by an
/// append-only file of `<unix_ts>\t<count>\t<path>` lines, compacted to one
/// line per path when it grows past [`COMPACT_THRESHOLD`] lines.
pub struct Frecency {
    map: HashMap<String, Entry>,
    file: PathBuf,
}

#[derive(Clone, Copy)]
struct Entry {
    count: u32,
    last: i64,
}

pub const COMPACT_THRESHOLD: usize = 20_000;

pub fn default_history_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".local")
                .join("state")
        });
    base.join("fsearch").join("history")
}

pub fn default_queries_path() -> PathBuf {
    default_history_path().with_file_name("queries")
}

/// Recent unique queries, oldest first, capped.
pub fn load_queries(path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let q = line.trim();
        if q.is_empty() {
            continue;
        }
        out.retain(|prev| prev != q); // keep most recent occurrence only
        out.push(q.to_string());
    }
    let excess = out.len().saturating_sub(100);
    out.drain(..excess);
    out
}

pub fn append_query(path: &std::path::Path, query: &str) {
    if query.trim().is_empty() {
        return;
    }
    if let Some(parent) = path.parent()
        && crate::util::create_private_dir(parent).is_err()
    {
        return;
    }
    if let Ok(mut f) = crate::util::append_private_file(path) {
        let _ = writeln!(f, "{}", query.trim());
    }
}

impl Frecency {
    pub fn load(file: PathBuf) -> Frecency {
        let mut map: HashMap<String, Entry> = HashMap::new();
        let mut lines = 0usize;
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text.lines() {
                lines += 1;
                let mut parts = line.splitn(3, '\t');
                let (Some(ts), Some(count), Some(path)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                let (Ok(ts), Ok(count)) = (ts.parse::<i64>(), count.parse::<u32>()) else {
                    continue;
                };
                let e = map
                    .entry(path.to_string())
                    .or_insert(Entry { count: 0, last: 0 });
                e.count = e.count.saturating_add(count);
                e.last = e.last.max(ts);
            }
        }
        let f = Frecency { map, file };
        if lines > COMPACT_THRESHOLD {
            f.compact();
        }
        f
    }

    /// Records an open at the current time.
    pub fn record(&mut self, path: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        self.record_at(path, now);
    }

    pub fn record_at(&mut self, path: &str, ts: i64) {
        let e = self
            .map
            .entry(path.to_string())
            .or_insert(Entry { count: 0, last: 0 });
        e.count = e.count.saturating_add(1);
        e.last = e.last.max(ts);
        if let Some(parent) = self.file.parent()
            && crate::util::create_private_dir(parent).is_err()
        {
            return;
        }
        if let Ok(mut f) = crate::util::append_private_file(&self.file) {
            let _ = writeln!(f, "{ts}\t1\t{path}");
        }
    }

    /// Ranking bonus per opened path: a recency bucket plus a capped
    /// open-count bonus. Sized to break fuzzy-score ties and to float
    /// opened files up in recency-ordered lists, not to drown out match
    /// quality.
    pub fn boosts(&self, now: i64) -> HashMap<String, u32> {
        self.map
            .iter()
            .map(|(path, e)| {
                let age = now.saturating_sub(e.last);
                let recency = match age {
                    a if a < 3600 => 48,
                    a if a < 24 * 3600 => 40,
                    a if a < 7 * 24 * 3600 => 28,
                    _ => 16,
                };
                (path.clone(), recency + e.count.min(10) * 4)
            })
            .collect()
    }

    fn compact(&self) {
        let mut body = String::new();
        for (path, e) in &self.map {
            body.push_str(&format!("{}\t{}\t{}\n", e.last, e.count, path));
        }
        // pid + counter temp name so concurrent fsearch processes compacting
        // at once never truncate each other's temp file
        let nonce = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .file
            .with_extension(format!("tmp-{}-{nonce}", std::process::id()));
        let Ok(mut f) = crate::util::create_private_file(&tmp) else {
            return;
        };
        let ok = (|| {
            f.write_all(body.as_bytes())?;
            // make the bytes durable before the rename publishes them
            f.sync_all()
        })();
        if ok.is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3600;

    #[test]
    fn queries_roundtrip_dedupe_and_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queries");
        append_query(&path, "alpha");
        append_query(&path, "beta");
        append_query(&path, "alpha"); // re-run: moves to most-recent
        append_query(&path, "  ");
        assert_eq!(load_queries(&path), vec!["beta", "alpha"]);
        for i in 0..150 {
            append_query(&path, &format!("q{i}"));
        }
        let qs = load_queries(&path);
        assert_eq!(qs.len(), 100);
        assert_eq!(qs.last().unwrap(), "q149");
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let f = Frecency::load(dir.path().join("history"));
        assert!(f.boosts(0).is_empty());
    }

    #[test]
    fn records_persist_and_accumulate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let mut f = Frecency::load(path.clone());
        f.record_at("/a/notes.md", 1000);
        f.record_at("/a/notes.md", 2000);
        f.record_at("/b/other.txt", 1500);
        drop(f);
        let f = Frecency::load(path);
        let boosts = f.boosts(2000);
        // twice-opened file gets a bigger boost than once-opened
        assert!(boosts["/a/notes.md"] > boosts["/b/other.txt"]);
    }

    #[test]
    fn recency_buckets_decay() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = Frecency::load(dir.path().join("history"));
        let now = 1_000_000_000;
        f.record_at("/recent", now - HOUR / 2);
        f.record_at("/today", now - 5 * HOUR);
        f.record_at("/thisweek", now - 3 * 24 * HOUR);
        f.record_at("/old", now - 60 * 24 * HOUR);
        let b = f.boosts(now);
        assert!(b["/recent"] > b["/today"]);
        assert!(b["/today"] > b["/thisweek"]);
        assert!(b["/thisweek"] > b["/old"]);
        assert!(b["/old"] > 0);
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        std::fs::write(&path, "garbage\n123\t1\t/ok\nbad\tline\n").unwrap();
        let f = Frecency::load(path);
        assert_eq!(f.boosts(123).len(), 1);
    }

    #[test]
    fn compaction_rewrites_one_line_per_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let mut body = String::new();
        for i in 0..(COMPACT_THRESHOLD + 100) {
            body.push_str(&format!("{}\t1\t/repeat/{}\n", 1000 + i as i64, i % 10));
        }
        std::fs::write(&path, body).unwrap();
        let f = Frecency::load(path.clone());
        // all opens counted…
        assert_eq!(f.boosts(2000).len(), 10);
        // …but the file now holds one compacted line per path
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert_eq!(lines, 10);
    }
}
