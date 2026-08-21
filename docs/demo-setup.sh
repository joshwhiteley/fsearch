#!/bin/sh
# Builds the demo tree that demo.tape records against, then pre-indexes it.
set -e
D=/tmp/fsearch-demo
rm -rf "$D"
mkdir -p "$D"/home/Documents/invoices "$D"/home/Documents/notes \
         "$D"/home/projects/demo/src "$D"/home/Pictures "$D"/xdg/fsearch "$D"/cache
cd "$D/home"
printf '# Invoice — Acme Corp — July\namount: $4,200\nstatus: overdue\nPayment was due 2026-07-31. Second reminder sent.\n' > Documents/invoices/invoice-acme-july.md
printf '# Invoice — Acme Corp — August\namount: $4,200\nstatus: paid\n' > Documents/invoices/invoice-acme-august.md
printf '# Invoice — Globex — Q2\namount: $11,850\nstatus: overdue\nEscalated to finance on 2026-08-12.\n' > Documents/invoices/invoice-globex-q2.md
printf '# Weekly sync\n- chase the overdue invoices\n- ship v0.7.0\n' > Documents/notes/meeting-notes.md
cat > projects/demo/src/main.rs <<'EOF'
use std::time::Instant;

/// Entry point: parse flags, load the cached index, run the UI.
fn main() {
    let start = Instant::now();
    let index = load_index().expect("index");
    println!("loaded {} paths in {:?}", index.len(), start.elapsed());
    run_ui(index);
}

fn load_index() -> Option<Vec<String>> {
    Some(vec!["~/notes.md".into(), "~/todo.txt".into()])
}

fn run_ui(paths: Vec<String>) {
    for p in paths.iter().take(3) {
        println!("hit: {p}");
    }
}
EOF
cat > Pictures/logo.svg <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="240" height="240">
  <rect width="240" height="240" rx="28" fill="#1a1b26"/>
  <circle cx="104" cy="104" r="56" fill="none" stroke="#7aa2f7" stroke-width="18"/>
  <rect x="146" y="146" width="72" height="22" rx="11" transform="rotate(45 146 146)" fill="#7aa2f7"/>
  <circle cx="104" cy="104" r="22" fill="#bb9af7"/>
</svg>
EOF
printf 'todo: renew passport\n' > todo.txt
printf '# demo home\n' > README.md
touch -t 202608200900 Documents/invoices/invoice-acme-august.md
touch -t 202607280900 Documents/invoices/invoice-acme-july.md
touch -t 202606150900 Documents/invoices/invoice-globex-q2.md
touch -t 202608190900 projects/demo/src/main.rs
touch -t 202605010900 Pictures/logo.svg
printf 'roots = ["/tmp/fsearch-demo/home"]\nindex_apps = false\n[theme]\npreset = "tokyonight"\nborders = "rounded"\n' > "$D/xdg/fsearch/config.toml"
HOME="$D/home" XDG_CONFIG_HOME="$D/xdg" XDG_CACHE_HOME="$D/cache" fsearch --reindex
