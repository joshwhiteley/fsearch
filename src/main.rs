use fsearch::cli::{self, Command};
use fsearch::{config, engine::Engine, index, tui, walker};
use std::io::{BufRead, IsTerminal};
use std::time::Instant;

fn main() {
    // The PDF and Office parsers are panic-guarded at their call sites; keep
    // the default hook from printing contained parser failures as crashes
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if fsearch::in_parser_guard() {
            return;
        }
        default_hook(info);
    }));
    match cli::parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Command::Run => {
            if !std::io::stdin().is_terminal() {
                // piped stdin: drop into filter mode automatically
                run_filter("")
            } else {
                run_ui(tui::UiMode::Open, "")
            }
        }
        Command::Help => print!("{}", cli::HELP),
        Command::Version => println!("fsearch {}", env!("CARGO_PKG_VERSION")),
        Command::Config => edit_config(),
        Command::Reindex => reindex(),
        Command::IndexSemantic => index_semantic(),
        Command::Doctor => doctor(),
        Command::Print(query) => print_search(&query),
        Command::Pick(initial) => run_ui(tui::UiMode::Pick, &initial),
        Command::Filter(initial) => run_filter(&initial),
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
        let (entries, _) = walker::collect_sorted(&config.roots, &excludes, config.index_apps);
        let _ = index::save(&entries, &cache);
        fsearch::index::PathStore::from_entries(&entries)
    })
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
        println!(
            "{:>10}  {}",
            fsearch::util::human_size(store.meta(i).size),
            store.get(i)
        );
    }
}

fn print_search(query: &str) {
    let config = load_config();
    let store = load_index(&config);
    let opts = fsearch::query::Options {
        max_content_filesize: config.max_content_filesize,
        quiet: fsearch::quiet::Quiet::new(config.quiet.clone()),
        pdf_cache: fsearch::pdf::default_cache_dir(),
    };
    let result = fsearch::query::search(&store, query, &opts, &mut |hit| match hit {
        fsearch::query::Hit::Path(path) => println!("{path}"),
        fsearch::query::Hit::Line {
            path,
            line_number,
            line,
        } => println!("{path}:{line_number}:{line}"),
        fsearch::query::Hit::Semantic {
            path,
            line_start,
            score,
        } => println!("{path}:{line_start}:{score:.2}"),
    });
    let matched = match result {
        Ok(any) => any,
        Err(e) => {
            eprintln!("fsearch: {e}");
            std::process::exit(2);
        }
    };
    if !matched {
        std::process::exit(1);
    }
}

/// Piped-stdin filter mode: launch the pick UI over arbitrary lines and
/// print the chosen line to stdout (exit 1 when nothing is chosen).
fn run_filter(initial_query: &str) {
    if std::io::stdin().is_terminal() {
        eprintln!(
            "fsearch: --filter reads lines from stdin (try: git ls-files | fsearch --filter)"
        );
        std::process::exit(2);
    }
    let lines: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .take(500_000)
        .collect();
    let config = load_config();
    // build the keymap and mouse flag before the engine consumes the config
    let keymap = fsearch::keymap::Keymap::from_config(&config.keys);
    let mouse = config.mouse;
    let theme = fsearch::theme::resolve_config(&config.theme);
    let engine = Engine::from_lines(lines);
    match tui::run(
        engine,
        tui::UiMode::Pick,
        initial_query,
        theme,
        keymap,
        mouse,
    ) {
        Ok(Some(picked)) => println!("{picked}"),
        Ok(None) => std::process::exit(1), // nothing chosen: signal like grep
        Err(e) => {
            eprintln!("fsearch: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run_ui(ui_mode: tui::UiMode, initial_query: &str) {
    let config = load_config();
    // build the keymap and mouse flag before the engine consumes the config
    let keymap = fsearch::keymap::Keymap::from_config(&config.keys);
    let mouse = config.mouse;
    let theme = fsearch::theme::resolve_config(&config.theme);
    let engine = Engine::new(
        config,
        index::default_cache_path(),
        fsearch::frecency::default_history_path(),
    );
    match tui::run(engine, ui_mode, initial_query, theme, keymap, mouse) {
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

/// Builds the vector index behind `?` queries: every text-bearing document
/// in the file index is chunked and embedded, reusing vectors for files
/// that haven't changed since the last run.
fn index_semantic() {
    let out_path = fsearch::sem::default_store_path();
    let prior = fsearch::sem::SemStore::load(&out_path);
    if let Some(legacy) = prior.as_ref().filter(|store| store.needs_migration()) {
        let start = Instant::now();
        if let Err(e) = legacy.save(&out_path) {
            eprintln!("fsearch: migrating {}: {e}", out_path.display());
            std::process::exit(1);
        }
        println!(
            "semantic index migrated to f16: {} documents, {} chunks in {:.1}s",
            legacy.docs.len(),
            legacy.chunk_count(),
            start.elapsed().as_secs_f32()
        );
        println!("run fsearch --index-semantic again to add new or changed documents");
        return;
    }

    let config = load_config();
    let store = load_index(&config);
    let mut embedder = match fsearch::sem::make_embedder() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("fsearch: {e}");
            std::process::exit(1);
        }
    };
    let files: Vec<(String, i64, u64)> = (0..store.len())
        .filter(|&i| {
            let p = store.get(i);
            !p.ends_with('/')
                && fsearch::sem::is_semantic_path(p)
                && store.meta(i).size <= fsearch::sem::MAX_SEMANTIC_BYTES
        })
        .map(|i| {
            (
                store.get(i).to_string(),
                store.meta(i).mtime,
                store.meta(i).size,
            )
        })
        .collect();
    let pdf_cache = fsearch::pdf::default_cache_dir();
    let office_cache = fsearch::office::default_cache_dir();
    let start = Instant::now();
    let mut read = |path: &str| -> Option<String> {
        if fsearch::pdf::is_pdf_path(path) {
            fsearch::pdf::extract_cached(path, &pdf_cache).ok()
        } else if fsearch::office::is_office_path(path) {
            fsearch::office::extract_cached(path, &office_cache).ok()
        } else {
            std::fs::read_to_string(path).ok()
        }
    };
    let mut last_tick = Instant::now();
    let mut progress = |done: usize, total: usize| {
        if last_tick.elapsed().as_millis() >= 500 {
            last_tick = Instant::now();
            eprint!("\r{done}/{total} documents");
        }
    };
    let result = fsearch::sem::build(
        &files,
        prior.as_ref(),
        embedder.as_mut(),
        &mut read,
        &mut progress,
    );
    let (sem_store, stats) = match result {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\rfsearch: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = sem_store.save(&out_path) {
        eprintln!("\rfsearch: writing {}: {e}", out_path.display());
        std::process::exit(1);
    }
    let secs = start.elapsed().as_secs_f32();
    print!(
        "\rsemantic index: {} documents, {} chunks ({} embedded, {} unchanged",
        sem_store.docs.len(),
        sem_store.chunk_count(),
        stats.embedded,
        stats.reused,
    );
    if stats.skipped > 0 {
        print!(", {} unreadable", stats.skipped);
    }
    println!(") in {secs:.1}s");
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
    let (entries, stats) = walker::collect_sorted(&config.roots, &excludes, config.index_apps);
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
