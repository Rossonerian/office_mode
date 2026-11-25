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
    // busy: three sessions side by side, a selected subagent next to a hovered session, long
    // bubbles in adjacent pods (the overlap case), agy lab calls.
    {
        use office_core::model::{AgentKind, AgentState};
        let mut s = office_core::Scene { now_ms: now, ..Default::default() };
        let mut add = |id: &str, kind: AgentKind, label: &str, parent: Option<&str>, state: AgentState, act: &str, start: u64| {
            let mut a = render::demo::demo_scene(now).agents.values().next().unwrap().clone();
            a.id = id.into();
            a.kind = kind;
            a.label = label.into();
            a.parent = parent.map(Into::into);
            a.state = state;
            a.activity = act.into();
            a.started_ms = start;
            a.last_event_ms = now - 500;
            a.cwd = Some(format!("/p/{label}"));
            a.role = (kind != AgentKind::Claude).then(|| label.split('·').next().unwrap_or(label).to_string());
            a.current_tool = (state == AgentState::NeedsYou).then(|| "AskUserQuestion".to_string());
            s.agents.insert(a.id.clone(), a);
        };
        add("cc:m", AgentKind::Claude, "cc·macoptimizer", None, AgentState::Idle, "", 1);
        add("cc-sub:m/1", AgentKind::ClaudeSub, "general-purpose", Some("cc:m"), AgentState::Running, "Bash syntax-check router as .ts", 2);
        add("cc:o", AgentKind::Claude, "cc·agent-office", None, AgentState::Running, "Bash test, reinstall, rerun doctor", 3);
        add("cc-sub:o/1", AgentKind::ClaudeSub, "agy-council-architect", Some("cc:o"), AgentState::Running, "Bash agy-route council-architect", 4);
        add("cc-sub:o/2", AgentKind::ClaudeSub, "agy-manager", Some("cc:o"), AgentState::Thinking, "starting…", 5);
        add("cc:d", AgentKind::Claude, "cc·DICE", None, AgentState::NeedsYou, "Bash rm -rf build", 6);
        add("agy:9", AgentKind::Agy, "worker·hydrogen-low", None, AgentState::Typing, "editing other-repo", 7);
        add("cc-sub:o/3", AgentKind::ClaudeSub, "agy-council-advisor", Some("cc:o"), AgentState::Running, "Bash agy-route council-advisor", 8);
        add("agy:11", AgentKind::Agy, "council-architect·neon-xhigh", Some("cc-sub:o/1"), AgentState::Thinking, "council-architect agent-office", 9);
        add("agy:12", AgentKind::Agy, "council-advisor·neon-high", Some("cc-sub:o/3"), AgentState::Thinking, "council-advisor agent-office", 10);
        // hand-offs in flight: a report going up, a brief going down
        let mk = |id: u64, at: u64, kind: office_core::ReportKind, from: &str, to: &str| office_core::Report {
            id, at_ms: at, kind, from: from.into(), from_label: from.into(), to: Some(to.into()), to_label: to.into(),
            title: "t".into(), body: String::new(), brief: String::new(),
        };
        s.reports.push(mk(1, now - 1_200, office_core::ReportKind::Report, "cc-sub:m/1", "cc:m"));
        s.reports.push(mk(2, now - 1_800, office_core::ReportKind::Task, "cc:o", "cc-sub:o/2"));
        r.set_mix(&ThemeMix { floor: "cozy".into(), walls: "cozy".into(), furniture: "cozy".into(), decor: "cozy".into(), characters: "procedural".into() });
        r.set_hover(Some("cc:o".into()));
        let sel = "cc-sub:m/1".to_string();
        for (name, (bw, bh)) in [("busy", (w, h)), ("busy-wide", (1600, 600)), ("busy-small", (640, 360))] {
            let img = r.compose(&s, Some(&sel), bw, bh, now);
            let p = out.join(format!("{name}.png"));
            img.save(&p)?;
            println!("{name:<11} {}", p.display());
        }
        // later: the session has reached the boss's desk with its plan, the vote is up
        let later = now + 9_000;
        s.now_ms = later;
        let img = r.compose(&s, Some(&sel), w, h, later);
        let p = out.join("busy-vote.png");
        img.save(&p)?;
        println!("busy-vote   {}", p.display());
        r.set_hover(None);
    }
    for e in r.theme_errors() {
        eprintln!("theme error: {e}");
    }
    Ok(())
}
