//! Render the demo scene for every theme to target/preview/<theme>.png and time compose().
//! Run: cargo run -p office --example preview [-- WIDTH HEIGHT]
#[path = "../src/render/mod.rs"]
#[allow(dead_code)]
mod render;

use office_core::config::{Config, ThemeMix};
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let args: Vec<u32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (w, h) = (args.first().copied().unwrap_or(1280), args.get(1).copied().unwrap_or(720));
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
    std::fs::create_dir_all(&out)?;
    let now = 10_000_000u64;
    let scene = render::demo::demo_scene(now);
    let mut r = render::Renderer::new(&Config::default());
    let mut themes = r.themes();
    themes.push("mixed".into());
    for t in &themes {
        let mix = if t == "mixed" {
            ThemeMix { floor: "cozy".into(), walls: "modern".into(), furniture: "modern".into(), decor: "cozy".into(), characters: "procedural".into() }
        } else {
            ThemeMix { floor: t.clone(), walls: t.clone(), furniture: t.clone(), decor: t.clone(), characters: t.clone() }
        };
        r.set_mix(&mix);
        let sel = "cc:beta".to_string();
        let img = r.compose(&scene, Some(&sel), w, h, now);
        let p = out.join(format!("{t}.png"));
        img.save(&p)?;
        // timing at 640×360 (static layer cached after the first frame)
        r.compose(&scene, None, 640, 360, now);
        let n = 60;
        let t0 = Instant::now();
        for i in 0..n {
            r.compose(&scene, Some(&sel), 640, 360, now + i * 83);
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
        println!("{t:<11} {} ({}x{} logical {:?}, scale {}) | 640x360: {ms:.2} ms/frame", p.display(), w, h, r.logical_size(), r.scale());
    }
    // ambient extras: an idle office (CEO leaning back / coffee), the cat hovered (name shown)
    r.set_mix(&ThemeMix::default());
    r.set_hover(Some(render::CAT_ID.to_string()));
    for (name, t) in [("ambient-lean", now + 13_000), ("ambient-sip", now + 23_000), ("ambient-back", now + 18_000)] {
        let idle = office_core::Scene { now_ms: t, ..Default::default() };
        let img = r.compose(&idle, None, w, h, t);
        let p = out.join(format!("{name}.png"));
        img.save(&p)?;
        println!("{name:<11} {}", p.display());
    }
    r.set_hover(None);
    for e in r.theme_errors() {
        eprintln!("theme error: {e}");
    }
    Ok(())
}
