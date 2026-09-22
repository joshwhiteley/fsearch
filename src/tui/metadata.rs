//! A single bounded metadata lane, independent of whether previews are shown.
use std::sync::mpsc;
use std::time::SystemTime;

type Metadata = Option<(bool, u64, Option<SystemTime>)>;
type Request = (u64, String);
type Reply = (u64, String, Metadata);

pub struct StatusCache {
    pub path: String,
    pub meta: Metadata,
    generation: u64,
    pending: Option<Request>,
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<Reply>,
}

impl StatusCache {
    pub(super) fn new() -> Self {
        Self::with_loader(|path| {
            std::fs::metadata(path)
                .ok()
                .map(|m| (m.is_file(), m.len(), m.modified().ok()))
        })
    }

    fn with_loader(load: impl Fn(&str) -> Metadata + Send + 'static) -> Self {
        let (tx, requests) = mpsc::sync_channel::<Request>(1);
        let (replies, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            while let Ok(mut request) = requests.recv() {
                // Skip queued work superseded before the previous stat ended.
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let meta =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(&request.1)))
                        .unwrap_or(None);
                if replies.send((request.0, request.1, meta)).is_err() {
                    break;
                }
            }
        });
        Self {
            path: String::new(),
            meta: None,
            generation: 0,
            pending: None,
            tx,
            rx,
        }
    }

    pub(super) fn refresh(&mut self, path: Option<String>) {
        let path = path.unwrap_or_default();
        if self.path != path {
            self.generation = self.generation.wrapping_add(1);
            self.path = path;
            self.meta = None;
            self.pending = (!self.path.is_empty()).then(|| (self.generation, self.path.clone()));
        }
        while let Ok((generation, path, meta)) = self.rx.try_recv() {
            if generation == self.generation && path == self.path {
                self.meta = meta;
            }
        }
        if let Some(request) = self.pending.take() {
            match self.tx.try_send(request) {
                Err(mpsc::TrySendError::Full(request)) => self.pending = Some(request),
                Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
            }
        }
    }
}

// Dropping both channel endpoints disconnects the worker, even if it was
// waiting to deliver a result. Do not join here: a filesystem stat can block
// indefinitely on a disconnected mount. At most one stat is in flight, and
// the detached worker exits as soon as that syscall returns.

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn metadata_is_off_thread_bounded_and_rejects_stale_same_path_replies() {
        let ui = std::thread::current().id();
        let (started, start_rx) = mpsc::channel();
        let (release, releases) = mpsc::channel();
        let mut cache = StatusCache::with_loader(move |path| {
            assert_ne!(ui, std::thread::current().id());
            started.send(path.to_string()).unwrap();
            let size = releases.recv().unwrap();
            Some((true, size, None))
        });
        cache.refresh(Some("a".into()));
        assert_eq!(start_rx.recv_timeout(Duration::from_secs(1)).unwrap(), "a");
        // The first load is blocked. UI refreshes never wait, and requests
        // remain bounded to one queued plus the latest pending selection.
        for i in 0..1000 {
            cache.refresh(Some(format!("b{i}")));
        }
        cache.refresh(Some("a".into()));
        release.send(1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            cache.refresh(Some("a".into()));
            assert_ne!(
                cache.meta,
                Some((true, 1, None)),
                "old a reply must not match new a generation"
            );
            if let Ok(path) = start_rx.try_recv() {
                release.send(2).unwrap();
                if path == "a" {
                    break;
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        while cache.meta.is_none() {
            cache.refresh(Some("a".into()));
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(cache.meta, Some((true, 2, None)));
        cache.refresh(None);
        assert_eq!(cache.meta, None);
    }
}
