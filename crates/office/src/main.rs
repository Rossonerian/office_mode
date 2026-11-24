//! office: terminal agent office. See docs/DESIGN.md. main.rs is owned by workstream C.
mod app;
mod cli;
mod render;
mod sources;

fn main() -> anyhow::Result<()> {
    let cfg = office_core::config::Config::load();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install-hooks") => cli::install_hooks(
            args.iter().any(|a| a == "--claude") || !args.iter().any(|a| a == "--antigravity"),
            args.iter().any(|a| a == "--antigravity") || !args.iter().any(|a| a == "--claude"),
            args.iter().any(|a| a == "--uninstall"),
        ),
        Some("doctor") => cli::doctor(),
        Some("demo") => app::run(cfg, true),
        _ => app::run(cfg, false),
    }
}
