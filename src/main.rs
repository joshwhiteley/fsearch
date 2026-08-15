use fsearch::cli::{self, Command};
use fsearch::{config, engine::Engine, index, tui, walker};
use std::time::Instant;

fn main() {
    match cli::parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Command::Run => run_ui(),
        Command::Help => print!("{}", cli::HELP),
        Command::Version => println!("fsearch {}", env!("CARGO_PKG_VERSION")),
        Command::Config => edit_config(),
        Command::Reindex => reindex(),
        Command::Doctor => doctor(),
        Command::Print(query) => print_search(&query),
        Command::Unknown(arg) => {
            eprintln!("fsearch: unexpected argument {arg:?}\n");
            eprint!("{}", cli::HELP);
            std::process::exit(2);
        }
    }
}

fn load_config() -> config::Config {
    match config::load_or_create(&config::default_config_path()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fsearch: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Prints what the terminal probe detects — for debugging preview quality
/// in multiplexers and unusual terminals.
fn doctor() {
    let (traits, picker) = tui::probe_terminal();
    println!("terminal answers queries: {}", traits.responsive);
    println!("background: {:?}", traits.appearance);
    match picker {
        Some(p) => {
            let f = p.font_size();
            println!("image protocol: {:?}", p.protocol_type());
            println!("cell size: {}x{} px", f.width, f.height);
            #[cfg(feature = "chafa")]
            println!("halfblock renderer: chafa");
            #[cfg(not(feature = "chafa"))]
            println!(
                "halfblock renderer: builtin (rebuild with --features chafa for sharper cells)"
            );
        }
        None => println!("image previews: off (FSEARCH_IMAGES=off)"),
    }
}

/// Non-interactive search: prints matches to stdout, exits 1 when none.
/// Uses the cached index when present; builds (and saves) one otherwise.
fn print_search(query: &str) {
    let config = load_config();
    let cache = index::default_cache_path();
    let paths = index::load(&cache).unwrap_or_else(|| {
        let excludes = walker::build_exclude_set(&config.excludes).unwrap_or_else(|e| {
            eprintln!("fsearch: invalid exclude pattern: {e:#}");
            std::process::exit(1);
        });
        let (paths, _) = walker::collect_sorted(&config.roots, &excludes);
        let _ = index::save(&paths, &cache);
        paths
    });

    let matched = if let Some(pattern) = query.strip_prefix('>') {
        let pattern = pattern.trim_start().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let max = config.max_content_filesize;
        let result = std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let r = fsearch::content::search(&paths, &pattern, max, &cancel, &tx);
                drop(tx);
                r
            });
            let mut any = false;
            for hit in rx {
                any = true;
                println!("{}:{}:{}", hit.path, hit.line_number, hit.line);
            }
            handle.join().expect("content search panicked").map(|_| any)
        });
        match result {
            Ok(any) => any,
            Err(e) => {
                eprintln!("fsearch: invalid pattern: {e}");
                std::process::exit(2);
            }
        }
    } else {
        match fsearch::matcher::search(&paths, query, fsearch::matcher::FilenameMode::Fuzzy, 500) {
            Ok(indices) => {
                for i in &indices {
                    println!("{}", paths[*i]);
                }
                !indices.is_empty()
            }
            Err(e) => {
                eprintln!("fsearch: {e}");
                std::process::exit(2);
            }
        }
    };
    if !matched {
        std::process::exit(1);
    }
}

fn run_ui() {
    let engine = Engine::new(
        load_config(),
        index::default_cache_path(),
        fsearch::frecency::default_history_path(),
    );
    if let Err(e) = tui::run(engine) {
        eprintln!("fsearch: {e:#}");
        std::process::exit(1);
    }
}

fn edit_config() {
    load_config(); // ensures the file exists with defaults
    let path = config::default_config_path();
    let visual = std::env::var("VISUAL").ok();
    let editor = std::env::var("EDITOR").ok();
    match cli::choose_config_open(visual.as_deref(), editor.as_deref()) {
        cli::ConfigOpen::Editor(editor) => {
            match std::process::Command::new(&editor).arg(&path).status() {
                Ok(s) if s.success() => {}
                Ok(s) => std::process::exit(s.code().unwrap_or(1)),
                Err(e) => {
                    eprintln!("fsearch: failed to run {editor}: {e}");
                    println!("{}", path.display());
                    std::process::exit(1);
                }
            }
        }
        cli::ConfigOpen::Reveal => {
            println!("{}", path.display());
            if let Err(e) = fsearch::actions::reveal(path.to_string_lossy().as_ref()) {
                eprintln!("fsearch: failed to reveal in Finder: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn reindex() {
    let config = load_config();
    let excludes = match walker::build_exclude_set(&config.excludes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fsearch: invalid exclude pattern: {e:#}");
            std::process::exit(1);
        }
    };
    let start = Instant::now();
    let (paths, stats) = walker::collect_sorted(&config.roots, &excludes);
    let cache = index::default_cache_path();
    if let Err(e) = index::save(&paths, &cache) {
        eprintln!("fsearch: writing {}: {e}", cache.display());
        std::process::exit(1);
    }
    let secs = start.elapsed().as_secs_f32();
    print!("indexed {} files in {secs:.1}s", stats.files);
    if stats.skipped > 0 {
        print!(" ({} unreadable entries skipped)", stats.skipped);
    }
    println!();
}
