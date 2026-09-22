use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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
const QUERY_LIMIT: usize = 100;
const QUERY_COMPACT_THRESHOLD: usize = QUERY_LIMIT;
const MAX_HISTORY_LINE: usize = 1024 * 1024;

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

fn lock_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{name}.lock"))
}

/// A sidecar lock is deliberately separate from the history file. Compaction
/// replaces the history inode, so locking that inode would let an append race
/// with the rename and lose an open.
fn lock_file(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        crate::util::create_private_dir(parent)?;
    }
    let lock = lock_path(path);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(lock)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("history lock is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.lock()?;
    Ok(file)
}

fn open_read(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("history is not a regular file"));
    }
    Ok(file)
}

/// Reads one line without allowing a malformed, unterminated record to grow
/// the buffer without bound. The returned bool says whether the line stayed
/// below the cap; oversized lines are consumed and reported as invalid.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut valid = true;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(valid));
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let take = newline.map_or(available.len(), |at| at + 1);
        let content_len = newline.unwrap_or(take);
        if valid {
            if content_len <= MAX_HISTORY_LINE.saturating_sub(line.len()) {
                line.extend_from_slice(&available[..content_len]);
            } else {
                valid = false;
            }
        }
        reader.consume(take);
        if newline.is_some() {
            return Ok(Some(valid));
        }
    }
}

fn load_queries_locked(path: &Path) -> (Vec<String>, usize) {
    let Ok(file) = open_read(path) else {
        return (Vec::new(), 0);
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut out = VecDeque::with_capacity(QUERY_LIMIT);
    let mut seen = HashSet::with_capacity(QUERY_LIMIT);
    let mut lines = 0;
    while let Ok(Some(valid)) = read_bounded_line(&mut reader, &mut line) {
        lines += 1;
        if !valid {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let query = text.trim();
        if query.is_empty() {
            continue;
        }
        if seen.remove(query)
            && let Some(position) = out.iter().position(|previous| previous == query)
        {
            out.remove(position);
        }
        seen.insert(query.to_string());
        out.push_back(query.to_string());
        if out.len() > QUERY_LIMIT {
            if let Some(oldest) = out.pop_front() {
                seen.remove(&oldest);
            }
        }
    }
    (out.into_iter().collect(), lines)
}

fn compact_queries(path: &Path, queries: &[String]) -> std::io::Result<()> {
    let nonce = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("queries-tmp-{}-{nonce}", std::process::id()));
    let mut file = crate::util::create_private_file(&tmp)?;
    let result = (|| {
        for query in queries {
            writeln!(file, "{query}")?;
        }
        file.sync_all()
    })();
    if result.is_ok() {
        let published = std::fs::rename(&tmp, path);
        if published.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        return published;
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Recent unique queries, oldest first, capped. Parsing and persisted state
/// stay bounded even if an old file contains malformed or duplicate records.
pub fn load_queries(path: &Path) -> Vec<String> {
    let Ok(_lock) = lock_file(path) else {
        return Vec::new();
    };
    let (queries, lines) = load_queries_locked(path);
    if lines > QUERY_LIMIT || lines != queries.len() {
        let _ = compact_queries(path, &queries);
    }
    queries
}

pub fn append_query(path: &Path, query: &str) {
    let query = query.trim();
    if query.is_empty()
        || query.len() > MAX_HISTORY_LINE
        || query.bytes().any(|byte| byte == b'\n' || byte == b'\r')
    {
        return;
    }
    let Ok(_lock) = lock_file(path) else {
        return;
    };
    let (mut queries, lines) = load_queries_locked(path);
    let old_len = queries.len();
    let duplicate = queries.iter().any(|previous| previous == query);
    queries.retain(|previous| previous != query);
    queries.push(query.to_string());
    if queries.len() > QUERY_LIMIT {
        queries.remove(0);
    }
    if lines >= QUERY_COMPACT_THRESHOLD || lines != old_len || duplicate {
        let _ = compact_queries(path, &queries);
    } else {
        // The lock covers both opening and writing; no append can race a
        // compacting rename because the lock lives beside the replaced inode.
        if let Ok(mut file) = crate::util::append_private_file(path) {
            let _ = writeln!(file, "{query}");
        }
    }
}

impl Frecency {
    pub fn load(file: PathBuf) -> Frecency {
        let mut map: HashMap<String, Entry> = HashMap::new();
        let mut lines = 0usize;
        if let Ok(_lock) = lock_file(&file)
            && let Ok(history) = open_read(&file)
        {
            let mut reader = BufReader::new(history);
            let mut line = Vec::new();
            while let Ok(Some(valid)) = read_bounded_line(&mut reader, &mut line) {
                lines += 1;
                if !valid {
                    continue;
                }
                let Ok(line) = std::str::from_utf8(&line) else {
                    continue;
                };
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
                    .entry(path.trim_end_matches('\r').to_string())
                    .or_insert(Entry { count: 0, last: 0 });
                e.count = e.count.saturating_add(count);
                e.last = e.last.max(ts);
            }
            let f = Frecency { map, file };
            if lines > COMPACT_THRESHOLD {
                f.compact_locked();
            }
            return f;
        }
        Frecency { map, file }
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
        if path.len() > MAX_HISTORY_LINE || path.bytes().any(|byte| byte == b'\n' || byte == b'\r')
        {
            return;
        }
        let Ok(_lock) = lock_file(&self.file) else {
            return;
        };
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

    fn compact_locked(&self) {
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
    fn malformed_query_records_do_not_escape_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queries");
        let mut body = vec![b'q'; MAX_HISTORY_LINE * 2];
        body.extend_from_slice(b"\nvalid\n");
        std::fs::write(&path, body).unwrap();
        assert_eq!(load_queries(&path), vec!["valid"]);
        append_query(&path, "next");
        assert_eq!(load_queries(&path), vec!["valid", "next"]);
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
    fn oversized_malformed_lines_are_bounded_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history");
        let mut body = vec![b'x'; MAX_HISTORY_LINE * 2];
        body.extend_from_slice(b"\n123\t1\t/ok\n");
        std::fs::write(&path, body).unwrap();
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
