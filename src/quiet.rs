//! "Quiet" paths: application internals that are technically files-on-disk
//! but almost never what a search is for — `~/Library/` state, log churn in
//! hidden dot-directories. Quiet paths stay fully searchable; they are
//! demoted below the weaker-matches fold and skipped on the launch screen,
//! unless the query shows path intent (a `/`, a `path:` filter, `dir:`).

/// Substring patterns marking a path as quiet.
pub struct Quiet {
    patterns: Vec<String>,
}

/// Default quiet markers: macOS app internals and hidden directories.
/// `/.` also matches plain dotfiles like `~/.zshrc` — that is fine, because
/// demotion is relative: when a dotfile is the best match for a query it
/// still ranks first (everything competing is equally quiet).
pub const DEFAULT_QUIET: &[&str] = &["/Library/", "/."];

impl Quiet {
    pub fn new(patterns: Vec<String>) -> Quiet {
        Quiet { patterns }
    }

    pub fn is_quiet(&self, path: &str) -> bool {
        self.patterns.iter().any(|p| path.contains(p.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

impl Default for Quiet {
    fn default() -> Quiet {
        Quiet::new(DEFAULT_QUIET.iter().map(|s| s.to_string()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_and_hidden_dirs_are_quiet() {
        let q = Quiet::default();
        assert!(q.is_quiet("/Users/j/Library/Biome/compute/sessions/x"));
        assert!(q.is_quiet("/Users/j/Library/Application Support/Firefox/y.sqlite"));
        assert!(q.is_quiet("/Users/j/.cisco/vpn/log/UIHistory.txt"));
        assert!(q.is_quiet("/Users/j/.zshrc"));
        assert!(!q.is_quiet("/Users/j/Documents/taxes/2026.pdf"));
        assert!(!q.is_quiet("/Applications/Safari.app"));
    }

    #[test]
    fn custom_patterns_replace_defaults() {
        let q = Quiet::new(vec!["/node_modules/".into()]);
        assert!(q.is_quiet("/Users/j/p/node_modules/x.js"));
        assert!(!q.is_quiet("/Users/j/Library/whatever"));
        assert!(Quiet::new(Vec::new()).is_empty());
    }
}
