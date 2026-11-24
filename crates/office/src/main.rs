//! office: terminal agent office. See docs/DESIGN.md. main.rs is owned by workstream C.
mod app;
mod cli;
mod render;
mod sources;

const USAGE: &str = "\
office: a terminal pixel office for your AI coding agents

USAGE:
    office                      run the office (live sources)
    office demo                 run with synthetic agents (no hooks needed)
    office install-hooks [--claude] [--antigravity] [--uninstall]
                                install (or remove) the office-hook entries
    office doctor               check hooks, socket, router and config
    office config               validate config.toml and print the effective settings
    office -h | --help          show this help
    office -V | --version       show the version

KEYS (in the office): q quit · ↑↓/jk select · enter details · a/d approve/deny
    x cancel agy call · o resume session · t/T themes · s sidebar · n notifications
    c clear alerts · ? help

CONFIG: ~/.config/agent-office/config.toml";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help" || a == "help") {
        println!("{USAGE}");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("office {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let cfg = office_core::config::Config::load();
    match args.first().map(String::as_str) {
        Some("install-hooks") => cli::install_hooks(
            args.iter().any(|a| a == "--claude") || !args.iter().any(|a| a == "--antigravity"),
            args.iter().any(|a| a == "--antigravity") || !args.iter().any(|a| a == "--claude"),
            args.iter().any(|a| a == "--uninstall"),
        ),
        Some("doctor") => cli::doctor(),
        Some("config") => {
            let path = office_core::paths::config_file();
            match std::fs::read_to_string(&path) {
                Err(_) => println!("# {} not found: using defaults", path.display()),
                Ok(s) => match toml::from_str::<office_core::config::Config>(&s) {
                    Ok(_) => println!("# {}: OK", path.display()),
                    Err(e) => {
                        eprintln!("# {}: INVALID, so the office falls back to defaults\n{e}", path.display());
                        std::process::exit(1);
                    }
                },
            }
            println!("{}", toml::to_string_pretty(&cfg)?);
            Ok(())
        }
        Some("demo") => app::run(cfg, true),
        None => app::run(cfg, false),
        Some(other) => {
            eprintln!("office: unknown command {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}
