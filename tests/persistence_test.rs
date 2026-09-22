use fsearch::frecency::{self, COMPACT_THRESHOLD, Frecency};
use std::process::Command;

#[test]
fn history_worker() {
    let Some(path) = std::env::var_os("FSEARCH_HISTORY_WORKER") else {
        return;
    };
    let id: usize = std::env::var("FSEARCH_HISTORY_WORKER_ID")
        .unwrap()
        .parse()
        .unwrap();
    let path = std::path::PathBuf::from(path);
    let mut history = Frecency::load(path);
    for n in 0..32 {
        history.record_at(&format!("/worker-{id}/file-{n}"), 10_000 + n as i64);
    }
}

#[cfg(unix)]
#[test]
fn concurrent_history_append_and_compaction_preserves_opens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history");
    let mut seed = String::new();
    for n in 0..=COMPACT_THRESHOLD {
        seed.push_str(&format!("1\t1\t/seed-{n}\n"));
    }
    std::fs::write(&path, seed).unwrap();

    let executable = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for id in 0..8 {
        children.push(
            Command::new(&executable)
                .args(["--exact", "history_worker", "--test-threads=1"])
                .env("FSEARCH_HISTORY_WORKER", &path)
                .env("FSEARCH_HISTORY_WORKER_ID", id.to_string())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        let status = child.wait().unwrap();
        assert!(status.success(), "history worker failed: {status}");
    }

    let history = Frecency::load(path);
    let boosts = history.boosts(20_000);
    for id in 0..8 {
        for n in 0..32 {
            assert!(
                boosts.contains_key(&format!("/worker-{id}/file-{n}")),
                "lost worker open {id}/{n}"
            );
        }
    }
}

#[test]
fn query_persistence_stays_capped_after_repeated_appends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queries");
    for n in 0..250 {
        frecency::append_query(&path, &format!("query-{n}"));
    }
    let queries = frecency::load_queries(&path);
    assert_eq!(queries.len(), 100);
    assert_eq!(queries.first().unwrap(), "query-150");
    assert_eq!(queries.last().unwrap(), "query-249");
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 100);
}
