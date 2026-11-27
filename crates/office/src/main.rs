//! office: terminal agent office. See docs/DESIGN.md. main.rs is owned by workstream C.
use office::{app, cli};

const USAGE: &str = "\
office (office_mode): pixel-art terminal office for your AI coding agents

USAGE:
    office                      run the office (live sources)
    office demo                 run with synthetic agents (no hooks needed)
    office desktop [--demo]     launch the native desktop app
    office import theme|sprite|ceo-outfit|sound ...
                                import custom themes, sprites, CEO outfits, or WAV sounds
    office install-hooks [--claude] [--antigravity] [--permission] [--uninstall]
                                install (or remove) the office-hook entries
    office doctor               check hooks, socket, themes, desktop, audio, outfits, whiteboard, router and config
    office config               validate config.toml and print the effective settings
    office -h | --help          show this help
    office -V | --version       show the version

KEYS (in the office): q quit · ↑↓/jk select · enter details · a/d approve/deny
    Space actions palette · x cancel · K kill idle · o resume · p dispatch · f follow-up
    r/i reports/inbox · b whiteboard · m sound · t/T themes · v view menu · s sidebar
    n notifications · c clear alerts · L labels · ? help

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
        Some("desktop") => cli::launch_desktop(args.iter().any(|a| a == "--demo")),
        Some("import") => cli::import_asset(&args[1..]),
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
