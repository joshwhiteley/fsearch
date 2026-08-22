pub mod actions;
pub mod calc;
pub mod cellart;
pub mod cli;
pub mod config;
pub mod content;
pub mod engine;
pub mod filters;
pub mod frecency;
pub mod highlight;
pub mod images;
pub mod index;
pub mod keymap;
pub mod matcher;
pub mod office;
pub mod pdf;
pub mod query;
pub mod quiet;
pub mod sem;
pub mod session;
pub mod theme;
pub mod tui;
pub mod util;
pub mod walker;

/// True while a panic-guarded parser (pdf-extract, office XML) runs on this
/// thread. Panic hooks check it so contained parser failures stay silent
/// instead of spraying over the UI (or tearing the terminal down).
pub fn in_parser_guard() -> bool {
    pdf::in_extract_guard() || office::in_extract_guard()
}
