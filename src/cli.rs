#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Run,
    Help,
    Version,
    Config,
    Reindex,
    Unknown(String),
}

pub const HELP: &str = "\
fsearch — fast Alfred-style file search for the terminal

usage:
  fsearch              launch the interactive search ui
  fsearch --config     open the config file in $VISUAL/$EDITOR,
                       or reveal it in Finder when neither is set
  fsearch --reindex    rebuild the file index now
  fsearch --help       show this help
  fsearch --version    print the version

query syntax:
  plain text           fuzzy match on file names and paths
  ctrl-r               toggle regex match on the full path
  > pattern            regex search inside file contents

keys:
  up/down, ctrl-k/j    move selection
  enter                open with default app
  ctrl-f               reveal in Finder
  ctrl-y               copy path to clipboard
  ctrl-u               clear query
  tab                  toggle preview pane
  esc, ctrl-c          quit

files:
  config               ~/.config/fsearch/config.toml
  index cache          ~/.cache/fsearch/index.bin
";

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigOpen {
    Editor(String),
    Reveal,
}

/// $VISUAL wins over $EDITOR; with neither set, reveal the file in Finder.
pub fn choose_config_open(visual: Option<&str>, editor: Option<&str>) -> ConfigOpen {
    visual
        .into_iter()
        .chain(editor)
        .find(|e| !e.is_empty())
        .map_or(ConfigOpen::Reveal, |e| ConfigOpen::Editor(e.to_string()))
}

/// Parses argv (without the program name) into a command.
pub fn parse(args: &[String]) -> Command {
    let mut cmd = Command::Run;
    for arg in args {
        let next = match arg.as_str() {
            "--help" | "-h" => Command::Help,
            "--version" | "-V" => Command::Version,
            "--config" => Command::Config,
            "--reindex" => Command::Reindex,
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
            "ctrl-r",
        ] {
            assert!(HELP.contains(needle), "help is missing {needle:?}");
        }
    }
}
