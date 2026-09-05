#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Run,
    Help,
    Version,
    Config,
    Reindex,
    IndexSemantic,
    Doctor,
    Status,
    ClearCache,
    ClearHistory,
    Searches,
    Print(String),
    Pick(String),
    Filter(String),
    Big(usize),
    Unknown(String),
}

pub const HELP: &str = "\
fsearch — fast Alfred-style file search for the terminal

usage:
  fsearch              launch the interactive search ui
  fsearch --config     open the config file in $VISUAL/$EDITOR,
                       or reveal it in the desktop file manager when
                       neither is set
  fsearch --reindex    rebuild the file index now
  fsearch --index-semantic
                       build/refresh the semantic index for ? queries
                       (text, PDF and Office docs; changed files only)
  fsearch --big [N]    largest N files in the index (default 20)
  fsearch -p QUERY     print matches to stdout (no ui); \"> pattern\"
                       searches file contents
  fsearch --pick [Q]   interactive ui, but enter prints the selection to
                       stdout instead of opening it (for scripts/pipes)
  fsearch --filter [Q] fuzzy-filter stdin lines and print the selection
  cat list | fsearch   same as --filter: piped stdin fuzzy-filters lines
  fsearch --doctor     print what the terminal probe detected
  fsearch --status     inspect index health without probing the terminal
  fsearch --clear-cache
                       remove search/extracted-text caches (keep models)
  fsearch --clear-history
                       remove open/query history and remembered layout
  fsearch --searches   list configured [searches]

options (before the command/query):
  --json               JSON records for -p/--big/--status/--pick/--filter
  --print0             NUL-terminated paths/selections (not JSON)
  --read0              NUL-separated stdin records (filter mode)
  --saved NAME         prepend a named [searches] query or scope
  --no-history         do not load/save history or session layout
  fsearch --help       show this help
  fsearch --version    print the version

query syntax:
  plain text           fuzzy match on file names and paths
  ctrl-r               toggle regex match on the full path
  'word                exact substring (^word prefix, word$ suffix,
                       !word excludes)
  > pattern            regex search inside files (PDF/DOCX/XLSX included)
  = 2*(3+4)            inline calculator (enter copies the result)
  ? words              search documents by meaning (semantic; run
                       fsearch --index-semantic first)
  ext:pdf path:term    narrow any search by extension or path
  kind:image           extension shorthands: image, video, audio,
                       doc, code, archive
  changed:7d           only files modified in the window (m/h/d/w)
  larger:100mb         size bounds (also smaller:)
  dir:                 search folders instead of files

keys:
  up/down, ctrl-k/j    move selection
  enter                open with default app
  ctrl-f               reveal in the file manager
  ctrl-y               copy path to clipboard
  ctrl-u               clear query
  tab                  cycle preview: side, full-window, hidden
  ctrl-t               toggle row density (two-line / compact)
  ctrl-x               show or hide weaker matches
  ctrl-s               mark file + move down (open mode)
  alt-s                clear marks (open mode)
  ctrl-g               cycle theme preset (session-only)
  f1, ctrl-o           open the help overlay
  esc, ctrl-c          quit
  all bindings         remappable via [keys] in config.toml

files:
  config               ~/.config/fsearch/config.toml
  index cache          ~/.cache/fsearch/index.bin
";

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigOpen {
    Editor(String),
    Reveal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Nul,
}

#[derive(Debug, PartialEq)]
pub struct Invocation {
    pub command: Command,
    pub format: OutputFormat,
    pub read0: bool,
    pub saved: Option<String>,
    pub no_history: bool,
}

/// Options precede the command. Everything after -p/--pick/--filter remains
/// query text, so a filename containing a flag is never silently rewritten.
pub fn parse_invocation(args: &[String]) -> Result<Invocation, String> {
    let mut format = OutputFormat::Text;
    let mut read0 = false;
    let mut no_history = false;
    let mut saved = None;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--json" | "--print0" => {
                let next = if arg == "--json" {
                    OutputFormat::Json
                } else {
                    OutputFormat::Nul
                };
                if format != OutputFormat::Text && format != next {
                    return Err("--json and --print0 cannot be combined".into());
                }
                format = next;
            }
            "--read0" => read0 = true,
            "--no-history" => no_history = true,
            "--saved" => {
                i += 1;
                let name = args
                    .get(i)
                    .filter(|s| !s.is_empty() && !s.starts_with('-'))
                    .ok_or("--saved needs a search name")?;
                if saved.replace(name.clone()).is_some() {
                    return Err("--saved can be supplied only once".into());
                }
            }
            _ => break,
        }
        i += 1;
    }
    let command = parse(&args[i..]);
    if read0 && !matches!(command, Command::Run | Command::Filter(_)) {
        return Err("--read0 requires filter mode".into());
    }
    let searching = matches!(
        command,
        Command::Run | Command::Print(_) | Command::Pick(_) | Command::Filter(_)
    );
    if saved.is_some() && !searching {
        return Err("--saved requires a search command".into());
    }
    if format != OutputFormat::Text
        && !(searching
            || (format == OutputFormat::Json
                && matches!(command, Command::Status | Command::Big(_))))
    {
        return Err("output option is not supported by this command".into());
    }
    Ok(Invocation {
        command,
        format,
        read0,
        saved,
        no_history,
    })
}

/// $VISUAL wins over $EDITOR; with neither set, reveal the file in the
/// desktop file manager.
pub fn choose_config_open(visual: Option<&str>, editor: Option<&str>) -> ConfigOpen {
    visual
        .into_iter()
        .chain(editor)
        .find(|e| !e.is_empty())
        .map_or(ConfigOpen::Reveal, |e| ConfigOpen::Editor(e.to_string()))
}

/// Parses argv (without the program name) into a command.
pub fn parse(args: &[String]) -> Command {
    if let Some(first) = args.first()
        && (first == "-p" || first == "--print")
    {
        let query = args[1..].join(" ");
        return if query.is_empty() {
            Command::Unknown(first.clone())
        } else {
            Command::Print(query)
        };
    }
    if let Some(first) = args.first()
        && first == "--pick"
    {
        return Command::Pick(args[1..].join(" "));
    }
    if let Some(first) = args.first()
        && first == "--filter"
    {
        return Command::Filter(args[1..].join(" "));
    }
    if let Some(first) = args.first()
        && first == "--big"
    {
        return match args.get(1) {
            None => Command::Big(20),
            Some(n) => match n.parse() {
                Ok(n) if args.len() == 2 => Command::Big(n),
                _ => Command::Unknown(args[1].clone()),
            },
        };
    }
    let mut cmd = Command::Run;
    for arg in args {
        let next = match arg.as_str() {
            "--help" | "-h" => Command::Help,
            "--version" | "-V" => Command::Version,
            "--config" => Command::Config,
            "--reindex" => Command::Reindex,
            "--index-semantic" => Command::IndexSemantic,
            "--doctor" => Command::Doctor,
            "--status" => Command::Status,
            "--clear-cache" => Command::ClearCache,
            "--clear-history" => Command::ClearHistory,
            "--searches" => Command::Searches,
            other => return Command::Unknown(other.to_string()),
        };
        if cmd != Command::Run {
            // two commands given; the second one is unexpected
            return Command::Unknown(arg.clone());
        }
        cmd = next;
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Command {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn no_args_runs_the_ui() {
        assert_eq!(parse_strs(&[]), Command::Run);
    }

    #[test]
    fn invocation_options_leave_query_arguments_literal() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let inv = parse_invocation(&args(&[
            "--json",
            "--no-history",
            "--saved",
            "docs",
            "-p",
            "--print0",
        ]))
        .unwrap();
        assert_eq!(inv.format, OutputFormat::Json);
        assert_eq!(inv.command, Command::Print("--print0".into()));
        assert!(inv.no_history);
        assert_eq!(inv.saved.as_deref(), Some("docs"));
        for invalid in [
            vec!["--json", "--print0", "-p", "q"],
            vec!["--read0", "-p", "q"],
            vec!["--saved"],
            vec!["--saved", "x", "--reindex"],
            vec!["--json", "--clear-cache"],
            vec!["--print0", "--status"],
        ] {
            assert!(parse_invocation(&args(&invalid)).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn help_flags() {
        assert_eq!(parse_strs(&["--help"]), Command::Help);
        assert_eq!(parse_strs(&["-h"]), Command::Help);
    }

    #[test]
    fn version_flags() {
        assert_eq!(parse_strs(&["--version"]), Command::Version);
        assert_eq!(parse_strs(&["-V"]), Command::Version);
    }

    #[test]
    fn config_and_reindex_flags() {
        assert_eq!(parse_strs(&["--config"]), Command::Config);
        assert_eq!(parse_strs(&["--reindex"]), Command::Reindex);
        assert_eq!(parse_strs(&["--index-semantic"]), Command::IndexSemantic);
    }

    #[test]
    fn print_takes_the_rest_as_query() {
        assert_eq!(
            parse_strs(&["-p", "tax", "2025"]),
            Command::Print("tax 2025".to_string())
        );
        assert_eq!(
            parse_strs(&["--print", "> needle"]),
            Command::Print("> needle".to_string())
        );
        // no query is an error
        assert_eq!(parse_strs(&["-p"]), Command::Unknown("-p".to_string()));
    }

    #[test]
    fn big_takes_an_optional_count() {
        assert_eq!(parse_strs(&["--big"]), Command::Big(20));
        assert_eq!(parse_strs(&["--big", "5"]), Command::Big(5));
        assert_eq!(
            parse_strs(&["--big", "nope"]),
            Command::Unknown("nope".to_string())
        );
    }

    #[test]
    fn pick_takes_an_optional_initial_query() {
        assert_eq!(parse_strs(&["--pick"]), Command::Pick(String::new()));
        assert_eq!(
            parse_strs(&["--pick", "dir:"]),
            Command::Pick("dir:".to_string())
        );
    }

    #[test]
    fn filter_takes_an_optional_initial_query() {
        assert_eq!(parse_strs(&["--filter"]), Command::Filter(String::new()));
        assert_eq!(
            parse_strs(&["--filter", "dir:"]),
            Command::Filter("dir:".to_string())
        );
    }

    #[test]
    fn anything_else_is_unknown() {
        assert_eq!(
            parse_strs(&["--bogus"]),
            Command::Unknown("--bogus".to_string())
        );
        // extra args after a valid flag are also rejected
        assert_eq!(
            parse_strs(&["--help", "x"]),
            Command::Unknown("x".to_string())
        );
    }

    #[test]
    fn config_opens_visual_then_editor_then_finder() {
        assert_eq!(
            choose_config_open(Some("code -w"), Some("vim")),
            ConfigOpen::Editor("code -w".to_string())
        );
        assert_eq!(
            choose_config_open(None, Some("vim")),
            ConfigOpen::Editor("vim".to_string())
        );
        assert_eq!(choose_config_open(None, None), ConfigOpen::Reveal);
        // empty strings count as unset
        assert_eq!(choose_config_open(Some(""), Some("")), ConfigOpen::Reveal);
    }

    #[test]
    fn help_text_covers_the_essentials() {
        for needle in [
            "usage",
            "--config",
            "--reindex",
            "--version",
            "> pattern",
            "? words",
            "--index-semantic",
            "ctrl-r",
            "ctrl-t",
            "ctrl-g",
            "f1, ctrl-o",
            "ctrl-s",
            "alt-s",
        ] {
            assert!(HELP.contains(needle), "help is missing {needle:?}");
        }
    }
}
