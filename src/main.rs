use fsearch::cli::{self, Command};
use fsearch::{config, engine::Engine, index, tui, walker};
use std::time::Instant;

fn main() {
    match cli::parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Command::Run => run_ui(tui::UiMode::Open, ""),
        Command::Help => print!("{}", cli::HELP),
        Command::Version => println!("fsearch {}", env!("CARGO_PKG_VERSION")),
        Command::Config => edit_config(),
        Command::Reindex => reindex(),
        Command::Doctor => doctor(),
        Command::Print(query) => print_search(&query),
        Command::Pick(initial) => run_ui(tui::UiMode::Pick, &initial),
        Command::Big(n) => biggest(n),
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
/// Loads the cached index, building (and saving) it when absent.
fn load_index(config: &config::Config) -> fsearch::index::PathStore {
    let cache = index::default_cache_path();
    index::load(&cache).unwrap_or_else(|| {
        let excludes = walker::build_exclude_set(&config.excludes).unwrap_or_else(|e| {
            eprintln!("fsearch: invalid exclude pattern: {e:#}");
            std::process::exit(1);
        });
        let (entries, _) = walker::collect_sorted(&config.roots, &excludes);
        let _ = index::save(&entries, &cache);
        fsearch::index::PathStore::from_entries(&entries)
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// "412 B", "1.3 KB", "2.0 MB", "1.1 GB" — mirrors the status line.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The N largest files in the index — a quick "what is eating my disk".
fn biggest(n: usize) {
    let config = load_config();
    let store = load_index(&config);
    let mut order: Vec<usize> = (0..store.len())
        .filter(|&i| !store.get(i).ends_with('/'))
        .collect();
    order.sort_by_key(|&i| std::cmp::Reverse(store.meta(i).size));
    for &i in order.iter().take(n) {
        println!("{:>10}  {}", human_size(store.meta(i).size), store.get(i));
    }
}

fn print_search(query: &str) {
    let config = load_config();
    let store = load_index(&config);

    let (query_filters, stripped) = fsearch::filters::parse(query, unix_now());
    let query = if query_filters.is_empty() {
        query.to_string()
    } else {
        stripped
    };
    let matched = if let Some(pattern) = query.strip_prefix('>') {
        let pattern = pattern.trim_start().to_string();
        let paths: Vec<String> = (0..store.len())
            .filter(|&i| {
                query_filters.is_empty()
                    || (query_filters.matches(store.get(i))
                        && query_filters.matches_meta(&store.meta(i)))
            })
            .map(|i| store.get(i).to_string())
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let max = config.max_content_filesize;
        let result = std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let pdf_cache = fsearch::pdf::default_cache_dir();
                let r = fsearch::content::search(&paths, &pattern, max, &pdf_cache, &cancel, &tx);
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
        match fsearch::matcher::search_boosted(
            &store,
            &query,
            fsearch::matcher::FilenameMode::Fuzzy,
            500,
            &std::collections::HashMap::new(),
            &query_filters,
        ) {
            Ok(indices) => {
                for i in &indices {
                    println!("{}", store.get(*i));
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

fn run_ui(ui_mode: tui::UiMode, initial_query: &str) {
    let config = load_config();
    let theme = fsearch::theme::resolve(&config.theme.preset, config.theme.accent.as_deref());
    let engine = Engine::new(
        config,
        index::default_cache_path(),
        fsearch::frecency::default_history_path(),
    );
    match tui::run(engine, ui_mode, initial_query, theme) {
        Ok(Some(picked)) => println!("{picked}"),
        Ok(None) => {
            if ui_mode == tui::UiMode::Pick {
                // nothing chosen: signal it like grep does
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("fsearch: {e:#}");
            std::process::exit(1);
        }
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
    let (entries, stats) = walker::collect_sorted(&config.roots, &excludes);
    let cache = index::default_cache_path();
    if let Err(e) = index::save(&entries, &cache) {
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
