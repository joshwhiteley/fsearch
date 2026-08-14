use fsearch::{config, engine::Engine, index, tui};

fn main() {
    let config = match config::load_or_create(&config::default_config_path()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fsearch: {e:#}");
            std::process::exit(1);
        }
    };
    let engine = Engine::new(config, index::default_cache_path());
    if let Err(e) = tui::run(engine) {
        eprintln!("fsearch: {e:#}");
        std::process::exit(1);
    }
}
