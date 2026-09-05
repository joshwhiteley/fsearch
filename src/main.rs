use fsearch::cli::{self, Command, Invocation, OutputFormat};
use fsearch::{config, engine::Engine, index, tui, walker};
use std::io::{BufRead, IsTerminal, Read, Write};
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
    let invocation = cli::parse_invocation(&std::env::args().skip(1).collect::<Vec<_>>())
        .unwrap_or_else(|error| fail(error));
    match &invocation.command {
        Command::Run => {
            if !std::io::stdin().is_terminal() {
                // piped stdin: drop into filter mode automatically
                run_filter("", &invocation)
            } else {
                if invocation.read0 || invocation.format != OutputFormat::Text {
                    fail("output/input record options require piped input or --pick/-p");
                }
                run_ui(tui::UiMode::Open, "", &invocation)
            }
        }
        Command::Help => write_stdout(cli::HELP),
        Command::Version => write_stdout(&format!("fsearch {}\n", env!("CARGO_PKG_VERSION"))),
        Command::Config => edit_config(),
        Command::Reindex => reindex(),
        Command::IndexSemantic => index_semantic(),
        Command::Doctor => doctor(),
        Command::Status => status(invocation.format),
        Command::ClearCache => clear_data(false),
        Command::ClearHistory => clear_data(true),
        Command::Searches => list_searches(),
        Command::Print(query) => print_search(query, &invocation),
        Command::Pick(initial) => run_ui(tui::UiMode::Pick, initial, &invocation),
        Command::Filter(initial) => run_filter(initial, &invocation),
        Command::Big(n) => biggest(*n, invocation.format),
        Command::Unknown(arg) => {
            eprintln!("fsearch: unexpected argument {arg:?}\n");
            eprint!("{}", cli::HELP);
            std::process::exit(2);
        }
    }
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("fsearch: {message}");
    std::process::exit(2)
}

fn load_run_config(invocation: &Invocation) -> config::Config {
    let mut config = load_config();
    if invocation.no_history {
        config.remember_history = false;
        config.remember_session = false;
    }
    config
}

fn saved_query(config: &config::Config, query: &str, name: Option<&str>) -> String {
    let Some(name) = name else {
        return query.to_string();
    };
    let saved = config
        .searches
        .get(name)
        .unwrap_or_else(|| fail(format!("unknown saved search {name:?}; use --searches")));
    combine_queries(saved, query).unwrap_or_else(|error| fail(error))
}

fn combine_queries(saved: &str, query: &str) -> Result<String, &'static str> {
    fn parts(query: &str) -> (Option<char>, &str) {
        match query.chars().next() {
            Some(prefix @ ('>' | '?' | '=')) => (Some(prefix), query[1..].trim_start()),
            _ => (None, query),
        }
    }
    let (saved_mode, saved_text) = parts(saved);
    let (query_mode, query_text) = parts(query);
    if saved_mode.is_some() && query_mode.is_some() && saved_mode != query_mode {
        return Err("saved search and supplied query use different modes");
    }
    let body = [saved_text, query_text]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(match query_mode.or(saved_mode) {
        Some(prefix) => format!("{prefix} {body}"),
        None => body,
    })
}

fn read_filter_input(reader: impl BufRead, nul: bool) -> Result<Vec<String>, String> {
    const MAX_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_RECORD: usize = 1024 * 1024;
    const MAX_RECORDS: usize = 500_000;
    let delimiter = if nul { b'\0' } else { b'\n' };
    let mut reader = reader.take(MAX_BYTES + 1);
    let mut total = 0;
    let mut records = Vec::new();
    loop {
        let mut bytes = Vec::new();
        let read = reader
            .read_until(delimiter, &mut bytes)
            .map_err(|e| format!("reading stdin: {e}"))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_BYTES || bytes.len() > MAX_RECORD {
            return Err("stdin exceeds the 64 MiB total or 1 MiB per-record limit".into());
        }
        if records.len() == MAX_RECORDS {
            eprintln!("fsearch: input truncated to {MAX_RECORDS} records");
            break;
        }
        if bytes.last() == Some(&delimiter) {
            bytes.pop();
            if !nul && bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }
        records.push(String::from_utf8(bytes).map_err(|_| "stdin is not valid UTF-8")?);
    }
    Ok(records)
}

fn status(format: OutputFormat) {
    let report = fsearch::health::inspect(&load_config(), &index::default_cache_path());
    if format == OutputFormat::Json {
        write_stdout(&format!(
            "{}\n",
            serde_json::to_string(&report).expect("health JSON")
        ));
    } else {
        write_stdout(&report.text());
    }
}

fn clear_data(history: bool) {
    let path = if history {
        fsearch::frecency::default_history_path()
    } else {
        index::default_cache_path()
    };
    let root = path.parent().expect("application data directory");
    let result = if history {
        fsearch::health::clear_history(root)
    } else {
        fsearch::health::clear_cache(root)
    };
    let removed = result.unwrap_or_else(|error| fail(error));
    write_stdout(&format!(
        "removed {removed} {} entries; close other fsearch instances to prevent recreation\n",
        if history { "history/session" } else { "cache" }
    ));
}

fn list_searches() {
    let config = load_config();
    let mut searches: Vec<_> = config.searches.iter().collect();
    searches.sort_by_key(|(name, _)| *name);
    for (name, query) in searches {
        write_stdout(&format!("{name}\t{query}\n"));
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

/// Writes text to stdout. A closed pipe (e.g. `fsearch --help | head`)
/// exits 0 quietly instead of panicking on the write.
fn write_stdout(text: &str) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(text.as_bytes()).and_then(|_| out.flush()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        eprintln!("fsearch: writing to stdout: {e}");
        std::process::exit(1);
    }
}

/// Prints what the terminal probe detects — for debugging preview quality
/// in multiplexers and unusual terminals.
fn doctor() {
    let (traits, picker) = tui::probe_terminal();
    let mut report = format!(
        "terminal answers queries: {}\nbackground: {:?}\n",
        traits.responsive, traits.appearance
    );
    match picker {
        Some(p) => {
            let f = p.font_size();
            report.push_str(&format!(
                "image protocol: {:?}\ncell size: {}x{} px\n",
                p.protocol_type(),
                f.width,
                f.height
            ));
            #[cfg(feature = "chafa")]
            report.push_str("halfblock renderer: chafa\n");
            #[cfg(not(feature = "chafa"))]
            report.push_str(
                "halfblock renderer: builtin (rebuild with --features chafa for sharper cells)\n",
            );
        }
        None => report.push_str("image previews: off (FSEARCH_IMAGES=off)\n"),
    }
    write_stdout(&report);
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
fn biggest(n: usize, format: OutputFormat) {
    let config = load_config();
    let store = load_index(&config);
    let mut order: Vec<usize> = (0..store.len())
        .filter(|&i| !store.get(i).ends_with('/'))
        .collect();
    order.sort_by_key(|&i| std::cmp::Reverse(store.meta(i).size));
    for &i in order.iter().take(n) {
        let line = if format == OutputFormat::Json {
            serde_json::json!({"type":"file", "path":store.get(i), "size":store.meta(i).size, "mtime":store.meta(i).mtime}).to_string()
        } else {
            format!(
                "{:>10}  {}",
                fsearch::util::human_size(store.meta(i).size),
                store.get(i)
            )
        };
        write_stdout(&format!("{line}\n"));
    }
}

fn print_search(query: &str, invocation: &Invocation) {
    let config = load_run_config(invocation);
    let query = saved_query(&config, query, invocation.saved.as_deref());
    let store = load_index(&config);
    let opts = fsearch::query::Options {
        max_content_filesize: config.max_content_filesize,
        quiet: fsearch::quiet::Quiet::new(config.quiet.clone()),
        pdf_cache: fsearch::pdf::default_cache_dir(),
    };
    // write_stdout survives broken pipes (`fsearch -p q | head` must exit 0)
    let result = fsearch::query::search(&store, &query, &opts, &mut |hit| {
        write_stdout(&fsearch::output::hit(hit, invocation.format));
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
/// print the chosen line to stdout (exit 1 when nothing is chosen, 2 on
/// real errors).
fn run_filter(initial_query: &str, invocation: &Invocation) {
    if std::io::stdin().is_terminal() {
        eprintln!(
            "fsearch: --filter reads lines from stdin (try: git ls-files | fsearch --filter)"
        );
        std::process::exit(2);
    }
    let lines = read_filter_input(std::io::stdin().lock(), invocation.read0)
        .unwrap_or_else(|error| fail(error));
    let config = load_run_config(invocation);
    let initial_query = saved_query(&config, initial_query, invocation.saved.as_deref());
    // build the keymap and mouse flag before the engine consumes the config
    let keymap = fsearch::keymap::Keymap::from_config(&config.keys);
    let mouse = config.mouse;
    let icons = config.icons;
    let remember_session = config.remember_session;
    let remember_history = config.remember_history;
    let theme_cfg = config.theme.clone();
    let engine = Engine::from_lines(lines);
    match tui::run(
        engine,
        tui::UiMode::Pick,
        &initial_query,
        theme_cfg,
        keymap,
        mouse,
        icons,
        remember_session,
        remember_history,
        Vec::new(),
        None,
    ) {
        Ok(Some(picked)) => write_stdout(&fsearch::output::selection(&picked, invocation.format)),
        Ok(None) => std::process::exit(1), // nothing chosen: signal like grep
        Err(e) => {
            eprintln!("fsearch: {e:#}");
            std::process::exit(2);
        }
    }
}

fn run_ui(ui_mode: tui::UiMode, initial_query: &str, invocation: &Invocation) {
    let config = load_run_config(invocation);
    let initial_query = saved_query(&config, initial_query, invocation.saved.as_deref());
    // build the keymap and mouse flag before the engine consumes the config
    let keymap = fsearch::keymap::Keymap::from_config(&config.keys);
    let mouse = config.mouse;
    let icons = config.icons;
    let remember_session = config.remember_session;
    let remember_history = config.remember_history;
    let theme_cfg = config.theme.clone();
    // no user-defined actions: offer detected-editor defaults instead
    let custom_actions = if config.actions.is_empty() {
        fsearch::config::default_actions()
    } else {
        config.actions.clone()
    };
    let action_warning = config.action_warning.clone();
    let engine = Engine::new(
        config,
        index::default_cache_path(),
        fsearch::frecency::default_history_path(),
    );
    match tui::run(
        engine,
        ui_mode,
        &initial_query,
        theme_cfg,
        keymap,
        mouse,
        icons,
        remember_session,
        remember_history,
        custom_actions,
        action_warning,
    ) {
        Ok(Some(picked)) => write_stdout(&fsearch::output::selection(&picked, invocation.format)),
        Ok(None) => {
            if ui_mode == tui::UiMode::Pick {
                // nothing chosen: signal it like grep does
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("fsearch: {e:#}");
            std::process::exit(2);
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
            let argv = shlex::split(&editor)
                .filter(|argv| !argv.is_empty())
                .unwrap_or_else(|| fail("invalid quoting in $VISUAL/$EDITOR"));
            match std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .arg(&path)
                .status()
            {
                Ok(s) if s.success() => {}
                Ok(s) => std::process::exit(s.code().unwrap_or(1)),
                Err(e) => {
                    eprintln!("fsearch: failed to run {editor}: {e}");
                    write_stdout(&format!("{}\n", path.display()));
                    std::process::exit(1);
                }
            }
        }
        cli::ConfigOpen::Reveal => {
            write_stdout(&format!("{}\n", path.display()));
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
    let prior_stamp = std::fs::metadata(&out_path).and_then(|m| m.modified()).ok();
    let mut prior = fsearch::sem::SemStore::load(&out_path);
    if let Some(legacy) = prior.as_ref().filter(|store| store.needs_migration()) {
        let start = Instant::now();
        if let Err(e) = legacy.save(&out_path) {
            eprintln!("fsearch: migrating {}: {e}", out_path.display());
            std::process::exit(1);
        }
        write_stdout(&format!(
            "semantic index migrated to f16: {} documents, {} chunks in {:.1}s\nrun fsearch --index-semantic again to add new or changed documents\n",
            legacy.docs.len(),
            legacy.chunk_count(),
            start.elapsed().as_secs_f32()
        ));
        return;
    }

    let config = load_config();
    // Refresh discovery and stat metadata before deciding what can be reused.
    // A cached path snapshot cannot establish semantic document freshness.
    let excludes = walker::build_exclude_set(&config.excludes).unwrap_or_else(|e| fail(e));
    let (entries, _) = walker::collect_sorted(&config.roots, &excludes, config.index_apps);
    index::save(&entries, &index::default_cache_path()).unwrap_or_else(|e| fail(e));
    let store = index::PathStore::from_entries(&entries);
    if let Some(prior) = &mut prior {
        // Stores record seconds. An actual file timestamp newer than the
        // previous store also invalidates reuse, catching equal-size edits
        // within the same second without changing the vector format.
        for doc in &mut prior.docs {
            let modified = std::fs::metadata(&doc.path).and_then(|m| m.modified()).ok();
            if modified.is_some_and(|modified| prior_stamp.is_none_or(|stamp| modified >= stamp)) {
                doc.mtime = i64::MIN;
            }
        }
    }
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
            let file = fsearch::util::open_regular_file(std::path::Path::new(path)).ok()?;
            if !file.metadata().ok()?.is_file() {
                return None;
            }
            let mut text = String::new();
            file.take(fsearch::sem::MAX_SEMANTIC_BYTES + 1)
                .read_to_string(&mut text)
                .ok()?;
            (text.len() as u64 <= fsearch::sem::MAX_SEMANTIC_BYTES).then_some(text)
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
    let mut summary = format!(
        "\rsemantic index: {} documents, {} chunks ({} embedded, {} unchanged",
        sem_store.docs.len(),
        sem_store.chunk_count(),
        stats.embedded,
        stats.reused
    );
    if stats.skipped > 0 {
        summary.push_str(&format!(", {} unreadable", stats.skipped));
    }
    summary.push_str(&format!(") in {secs:.1}s\n"));
    write_stdout(&summary);
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
    let mut summary = format!("indexed {} files in {secs:.1}s", stats.files);
    if stats.skipped > 0 {
        summary.push_str(&format!(" ({} unreadable entries skipped)", stats.skipped));
    }
    write_stdout(&format!("{summary}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nul_filter_records_preserve_newlines_and_spaces() {
        let input = b"a\nb\0ends-in-slash/\0  \0\0";
        assert_eq!(
            read_filter_input(&input[..], true).unwrap(),
            ["a\nb", "ends-in-slash/", "  ", ""]
        );
        assert_eq!(
            read_filter_input(&b"a\r\nb\n"[..], false).unwrap(),
            ["a", "b"]
        );
        assert!(read_filter_input(&b"invalid\xff\n"[..], false).is_err());
        assert!(read_filter_input(&vec![b'a'; 1024 * 1024 + 1][..], false).is_err());
    }
    #[test]
    fn saved_query_composition_preserves_modes_and_pattern_spacing() {
        assert_eq!(
            combine_queries("ext:md", "> two  spaces").unwrap(),
            "> ext:md two  spaces"
        );
        assert_eq!(
            combine_queries("> needle", "path:docs").unwrap(),
            "> needle path:docs"
        );
        assert_eq!(
            combine_queries("dir: path:work", "project").unwrap(),
            "dir: path:work project"
        );
        assert!(combine_queries("> needle", "? meaning").is_err());
    }
}
