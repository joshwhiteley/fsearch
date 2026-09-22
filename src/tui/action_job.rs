//! At most one clipboard/trash operation per app, with no unbounded queue.
use super::*;

pub(super) struct ActionJob {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<String>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ActionJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Subprocess loops observe cancellation, kill and reap the current
        // child, and skip any remaining batch files/backends.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl App {
    pub(super) fn start_action(
        &mut self,
        work: impl FnOnce(&AtomicBool) -> String + Send + 'static,
    ) {
        if self.action_job.is_some() {
            self.set_message("action already running".into());
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("file-action".into())
            .spawn(move || {
                let message = work(&worker_cancel);
                let _ = tx.send(message);
            }) {
            Ok(worker) => {
                self.action_job = Some(ActionJob {
                    cancel,
                    rx,
                    worker: Some(worker),
                });
                self.set_message("action running…".into());
            }
            Err(error) => self.set_message(format!("error starting action: {error}")),
        }
    }

    pub(super) fn poll_action(&mut self) {
        let Some(job) = &self.action_job else { return };
        let message = match job.rx.try_recv() {
            Ok(message) => message,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => "error: action worker stopped".into(),
        };
        drop(self.action_job.take());
        self.set_message(message);
    }

    pub(super) fn copy_selected(&mut self) {
        if let Some(path) = self.visible_selected_row().map(|row| row.path.clone()) {
            self.copy_async(path.clone(), format!("copied: {path}"));
        }
    }

    pub(super) fn copy_async(&mut self, text: String, success: String) {
        self.start_action(
            move |cancel| match actions::copy_with_cancel(&text, cancel) {
                Ok(()) => success,
                Err(_) if cancel.load(Ordering::Relaxed) => "copy cancelled".into(),
                Err(error) => format!("error copying: {error}"),
            },
        );
    }

    pub(super) fn trash_async(&mut self, paths: Vec<String>) {
        if paths.is_empty() {
            self.set_message("no visible marked files".into());
            return;
        }
        self.start_action(move |cancel| {
            let mut outcome = BatchOutcome {
                succeeded: 0,
                first_error: None,
            };
            let mut attempted = 0;
            for path in &paths {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                attempted += 1;
                match actions::trash_with_cancel(path, cancel) {
                    Ok(()) => outcome.succeeded += 1,
                    Err(error) if outcome.first_error.is_none() => {
                        outcome.first_error = Some((path.clone(), error.to_string()))
                    }
                    Err(_) => {}
                }
            }
            let mut message = batch_summary("trashed", attempted, &outcome);
            if attempted < paths.len() {
                message.push_str(&format!("; {} cancelled", paths.len() - attempted));
            }
            message
        });
    }
}
