//! PDF parsing only runs after exec, never in the searching process or between
//! fork and exec. Linux additionally enforces RLIMIT_AS (virtual address space).
//! Darwin's RLIMIT_AS aliases RSS and is not an enforceable address-space cap:
//! on both systems the helper's global allocator instead hard-caps Rust heap
//! allocations, including the parser's Rust decompression buffers. This is a
//! heap budget, not a claim to cap total RSS/native allocations. RLIMIT_CPU is
//! process CPU seconds on both systems; the parent also limits wall time and
//! output, kills and reaps on failure. No parsing fallback is allowed.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

const HEAP_BYTES: usize = 256 * 1024 * 1024;
const CPU_SECONDS: u64 = 3;
const WALL_SECONDS: u64 = 10;
const OUTPUT_BYTES: usize = crate::document_cache::MAX_ENTRY_BYTES as usize;
const HELPER_ARG: &str = "--internal-pdf-extract";

/// Install in binaries that dispatch PDF helpers. Accounting starts at process
/// startup, but the finite limit is enabled only in the isolated helper.
pub struct PdfAllocator {
    live: AtomicUsize,
    limit: AtomicUsize,
}

impl Default for PdfAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl PdfAllocator {
    pub const fn new() -> Self {
        Self {
            live: AtomicUsize::new(0),
            limit: AtomicUsize::new(usize::MAX),
        }
    }

    fn charge(layout: Layout) -> Option<usize> {
        // Include alignment slack and conservative per-allocation overhead.
        layout.size().checked_add(layout.align())?.checked_add(64)
    }

    fn reserve(&self, bytes: usize) -> bool {
        self.live
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |live| {
                live.checked_add(bytes)
                    .filter(|next| *next <= self.limit.load(Ordering::SeqCst))
            })
            .is_ok()
    }

    fn enable(&self) -> Result<(), &'static str> {
        self.limit.store(HEAP_BYTES, Ordering::SeqCst);
        if self.live.load(Ordering::SeqCst) > HEAP_BYTES {
            Err("PDF helper startup exceeds heap budget")
        } else {
            Ok(())
        }
    }
}

// SAFETY: allocation and deallocation are paired through System with their
// original layouts. Accounting failure returns null without touching memory.
unsafe impl GlobalAlloc for PdfAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some(charge) = Self::charge(layout) else {
            return std::ptr::null_mut();
        };
        if !self.reserve(charge) {
            return std::ptr::null_mut();
        }
        let ptr = unsafe { System.alloc(layout) };
        if ptr.is_null() {
            self.live.fetch_sub(charge, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(ptr, layout);
        }
        self.live
            .fetch_sub(Self::charge(layout).unwrap(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let Ok(next) = Layout::from_size_align(size, layout.align()) else {
            return std::ptr::null_mut();
        };
        let Some(charge) = Self::charge(next) else {
            return std::ptr::null_mut();
        };
        // Reserve the full new allocation, not just growth: System may briefly
        // retain both buffers. A failed realloc leaves the old charge intact.
        if !self.reserve(charge) {
            return std::ptr::null_mut();
        }
        let result = unsafe { System.realloc(ptr, layout, size) };
        self.live.fetch_sub(
            if result.is_null() {
                charge
            } else {
                Self::charge(layout).unwrap()
            },
            Ordering::SeqCst,
        );
        result
    }
}

#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: PdfAllocator = PdfAllocator::new();

/// Call before ordinary CLI parsing. The hidden helper consumes a regular PDF
/// on stdin; stderr is the bounded binary response channel, stdout is unused.
/// Library callers use an installed sibling fsearch executable. If unavailable,
/// extraction returns an error rather than parsing in an unbounded host.
pub fn dispatch(allocator: &PdfAllocator) {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == HELPER_ARG)
    {
        run_helper(allocator);
    }
}

fn run_helper(allocator: &PdfAllocator) -> ! {
    use std::io::{Read, Write};
    std::panic::set_hook(Box::new(|_| {}));
    let result: Result<String, String> = (|| {
        allocator.enable().map_err(str::to_owned)?;
        limits()?;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(crate::pdf::MAX_PDF_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > crate::pdf::MAX_PDF_BYTES {
            return Err("PDF grew beyond the input-size limit".into());
        }
        let text = crate::pdf::parse(&bytes)?;
        if text.len() > OUTPUT_BYTES {
            return Err("extracted PDF text exceeds 8 MiB".into());
        }
        Ok(text)
    })();
    let (tag, body) = match &result {
        Ok(text) => (0, text.as_str()),
        Err(error) => (1, error.as_str()),
    };
    let mut output = std::io::stderr().lock();
    let limit = if tag == 0 {
        OUTPUT_BYTES
    } else {
        crate::document_cache::MAX_ERROR_BYTES as usize
    };
    let mut end = body.len().min(limit);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let success = output
        .write_all(&[tag])
        .and_then(|_| output.write_all(&body.as_bytes()[..end]))
        .and_then(|_| output.flush())
        .is_ok();
    std::process::exit(if success { 0 } else { 1 });
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn limits() -> Result<(), String> {
    unsafe {
        let cpu = libc::rlimit {
            rlim_cur: CPU_SECONDS as libc::rlim_t,
            rlim_max: CPU_SECONDS as libc::rlim_t,
        };
        if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
            return Err(format!(
                "setting PDF CPU limit: {}",
                std::io::Error::last_os_error()
            ));
        }
        let core = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::setrlimit(libc::RLIMIT_CORE, &core) != 0 {
            return Err("setting PDF core dump limit failed".into());
        }
        #[cfg(target_os = "linux")]
        {
            let memory = libc::rlimit {
                rlim_cur: 768 * 1024 * 1024,
                rlim_max: 768 * 1024 * 1024,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &memory) != 0 {
                return Err(format!(
                    "setting PDF address-space limit: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn limits() -> Result<(), String> {
    Err("bounded PDF extraction is unsupported on this platform".into())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn extract(path: &str) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let file =
        crate::util::open_regular_file(std::path::Path::new(path)).map_err(|e| e.to_string())?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(test)]
    let mut command = {
        let mut command = Command::new(executable);
        command
            .args([
                "--exact",
                "pdf_process::tests::helper_entry",
                "--ignored",
                "--nocapture",
            ])
            .env("FSEARCH_PDF_TEST_CHILD", "1");
        command
    };
    #[cfg(not(test))]
    let mut command = {
        // Library callers use the installed fsearch sibling. Never run a
        // parser in the caller process. Cargo integration tests use ../fsearch.
        let helper = if executable.file_stem().is_some_and(|name| name == "fsearch") {
            executable
        } else {
            let beside = executable.with_file_name("fsearch");
            if beside.is_file() {
                beside
            } else {
                let in_parent = executable
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.join("fsearch"));
                in_parent
                    .filter(|p| p.is_file())
                    .ok_or("PDF helper unavailable; install fsearch beside the library host")?
            }
        };
        let mut command = Command::new(helper);
        command.arg(HELPER_ARG);
        command
    };
    command
        .env("RAYON_NUM_THREADS", "1")
        .stdin(Stdio::from(file))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|e| format!("starting bounded PDF helper: {e}"))?;
    let bytes = collect(
        child,
        std::time::Duration::from_secs(WALL_SECONDS),
        OUTPUT_BYTES + 1,
    )?;
    match bytes.split_first() {
        Some((tag @ (0 | 1), body)) => {
            let text = String::from_utf8(body.to_vec()).map_err(|_| "invalid PDF helper output")?;
            if *tag == 0 { Ok(text) } else { Err(text) }
        }
        _ => Err("invalid PDF helper response".into()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn extract(_path: &str) -> Result<String, String> {
    limits().map(|_| String::new())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn collect(
    mut child: std::process::Child,
    timeout: std::time::Duration,
    cap: usize,
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let result = (|| {
        let mut pipe = child
            .stderr
            .take()
            .ok_or("PDF helper has no response pipe")?;
        let fd = pipe.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err("setting PDF pipe nonblocking failed".into());
            }
        }
        let start = std::time::Instant::now();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            if start.elapsed() >= timeout {
                return Err("PDF extraction exceeded time limit".into());
            }
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                        return if status.success() {
                            Ok(bytes)
                        } else {
                            Err("PDF helper failed (parser crash or resource limit)".into())
                        };
                    }
                }
                Ok(n) => {
                    if bytes.len().saturating_add(n) > cap {
                        return Err("PDF helper exceeded output limit".into());
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("reading PDF helper: {e}")),
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    })();
    // Always reap, including read errors, timeout, output overflow, and panic
    // or allocation-abort exits. No reader/writer threads survive a timeout.
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "subprocess entry point, invoked only by the bounded parent"]
    fn helper_entry() {
        if std::env::var_os("FSEARCH_PDF_TEST_CHILD").is_some() {
            run_helper(&TEST_ALLOCATOR);
        }
    }

    #[test]
    fn allocator_accounts_failures_and_reallocations() {
        let allocator = PdfAllocator::new();
        let layout = Layout::from_size_align(32, 8).unwrap();
        let charge = PdfAllocator::charge(layout).unwrap();
        allocator.limit.store(charge, Ordering::SeqCst);
        unsafe {
            let ptr = allocator.alloc(layout);
            assert!(!ptr.is_null());
            assert!(allocator.alloc(layout).is_null());
            assert!(allocator.realloc(ptr, layout, 64).is_null());
            assert_eq!(allocator.live.load(Ordering::SeqCst), charge);
            allocator.limit.store(1024, Ordering::SeqCst);
            let ptr = allocator.realloc(ptr, layout, 64);
            assert!(!ptr.is_null());
            let next = Layout::from_size_align(64, 8).unwrap();
            assert_eq!(
                allocator.live.load(Ordering::SeqCst),
                PdfAllocator::charge(next).unwrap()
            );
            allocator.dealloc(ptr, next);
            assert_eq!(allocator.live.load(Ordering::SeqCst), 0);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[ignore = "resource probe, invoked only by the bounded parent"]
    fn heap_exhaustion_entry() {
        if std::env::var_os("FSEARCH_PDF_TEST_CHILD").is_none() {
            return;
        }
        TEST_ALLOCATOR.enable().unwrap();
        limits().unwrap();
        let mut retained = Vec::new();
        loop {
            retained.push(vec![42u8; 1024 * 1024]);
            std::hint::black_box(&retained);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[ignore = "resource probe, invoked only by the bounded parent"]
    fn cpu_exhaustion_entry() {
        if std::env::var_os("FSEARCH_PDF_TEST_CHILD").is_none() {
            return;
        }
        TEST_ALLOCATOR.enable().unwrap();
        limits().unwrap();
        loop {
            std::hint::black_box(42);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn resource_exhaustion_is_confined_to_exec_children() {
        for probe in ["heap_exhaustion_entry", "cpu_exhaustion_entry"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    &format!("pdf_process::tests::{probe}"),
                    "--ignored",
                    "--nocapture",
                ])
                .env("FSEARCH_PDF_TEST_CHILD", "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped());
            let error = collect(
                command.spawn().unwrap(),
                std::time::Duration::from_secs(8),
                8192,
            )
            .unwrap_err();
            assert!(error.contains("resource limit"), "{probe}: {error}");
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn decompression_bomb_is_contained_and_valid_pdf_still_works() {
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        let block = [b' '; 65536];
        // Build a 512 MiB expansion without ever materializing it in this
        // process. Only the exec child decompresses it, under external limits.
        for _ in 0..8192 {
            encoder.write_all(&block).unwrap();
        }
        let compressed = encoder.finish().unwrap();
        let mut bytes = crate::pdf::minimal_pdf("original");
        let offset = bytes.len();
        write!(
            bytes,
            "4 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed.len()
        )
        .unwrap();
        bytes.extend_from_slice(&compressed);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let xref = bytes.len();
        // An incremental update points object 4 at the compressed stream.
        let original = String::from_utf8(crate::pdf::minimal_pdf("original")).unwrap();
        let previous = original
            .split("startxref\n")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap();
        write!(bytes, "xref\n4 1\n{offset:010} 00000 n \ntrailer\n<< /Size 6 /Root 1 0 R /Prev {previous} >>\nstartxref\n{xref}\n%%EOF\n").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.pdf");
        std::fs::write(&path, bytes).unwrap();
        let error = extract(path.to_str().unwrap()).unwrap_err();
        assert!(
            error.contains("resource limit") || error.contains("time limit"),
            "{error}"
        );
        std::fs::write(&path, crate::pdf::minimal_pdf("still healthy")).unwrap();
        assert!(
            extract(path.to_str().unwrap())
                .unwrap()
                .contains("still healthy")
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parent_bounds_wall_time_and_output() {
        use std::process::{Command, Stdio};
        let mut sleeping = Command::new("/bin/sleep");
        sleeping.arg("5").stderr(Stdio::piped());
        assert!(
            collect(
                sleeping.spawn().unwrap(),
                std::time::Duration::from_millis(50),
                64
            )
            .unwrap_err()
            .contains("time limit")
        );
        let mut noisy = Command::new("/bin/sh");
        noisy
            .args(["-c", "printf '0123456789' >&2"])
            .stderr(Stdio::piped());
        assert!(
            collect(noisy.spawn().unwrap(), std::time::Duration::from_secs(2), 4)
                .unwrap_err()
                .contains("output limit")
        );
    }
}
