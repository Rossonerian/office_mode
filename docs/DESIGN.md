# agent-office: design contract

An original terminal "office" for AI coding agents (Claude Code sessions plus subagents, and agy calls via `~/.claude/bin/agy-route`). It shows who is working, what they are doing, cost and RAM, raises alerts, and lets you act (approve/deny, cancel, resume).

## Crates
- `office-core`: the contract. `model.rs` (Agent, Scene, Alert, RouterStatus), `event.rs` (Event/EventKind), `reducer.rs` (fold events into a Scene; stale sweep), `alerts.rs` (loop/stuck/cost detectors), `config.rs` (TOML config), `paths.rs`.
- `office-hook`: a tiny binary that agents call per hook event. It reads stdin JSON, tags `OFFICE_SOURCE` (claude|antigravity), and writes ONE line `{"source":..,"payload":<raw>,"ts":..}` to `paths::hook_socket()`. It ALWAYS exits 0 within 300 ms. With `--permission` it may wait (up to `permission_timeout_s`) for a decision from the TUI (see Control).
- `office`: the TUI binary (`office`). Subcommands: `office` (run), `office demo` (synthetic agents), `office install-hooks [--claude] [--antigravity] [--uninstall]`, `office doctor`.

## Sources (crates/office/src/sources/)
- `hook_socket.rs`: unix listener; decodes Claude hook payloads (SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, PostToolUseFailure, Notification, Stop, SubagentStart/SubagentStop, SessionEnd, PermissionRequest) and Antigravity hook payloads into Events. Claude subagents: hook payloads carry `agent_id`/`agent_type` when fired inside a subagent. Map them to `cc-sub:<session>/<agent_id>` with parent `cc:<session>` and role=agent_type.
- `claude_transcripts.rs`: tails ~/.claude/projects/**/*.jsonl (only files modified in the last 2h; first-sight gate) for usage (input/output/cache tokens), giving Usage events with estimated cost (a model price table in config) and context %.
- `agy_router.rs`: every 1 s, scans `ps` for `bun … agy-route --role <r> … --cwd <c>` (skipping `--dry-run`), sums each router's descendant RSS, and reads `~/.claude/run/agy-route.slot*.lock` and `agy-route.worker-*.lock`. It emits AgentSeen(kind=Agy, id="agy:<router pid>", role, model from the role→model map in config, slot, pid, label "<role>·<model short>"), Resources, and Gone when the pid disappears. Every 5 s it runs `agy-route --role fallback --prompt x --dry-run` and parses CAPACITY/FREE_MB/PER_PROC_MB/PRESSURE/SLOTS/LIMIT into a Router event. It tails `~/.claude/run/agy-route.log.jsonl` for finished calls: errorCode TOOL_DENIED/TIMEOUT/EMPTY_RESPONSE/BUSY become Alert events. The agy model per role is in the router's audit lines (`role`, `model`).

## Rendering (crates/office/src/render/)
- The scene is composed into an RGBA framebuffer (the `image` crate) from a Theme: floor/wall tiles, desks with computers, decor, characters. Displayed via `ratatui-image` (Kitty graphics in Ghostty, then Sixel/iTerm2, then half-blocks as the universal fallback).
- Layout: auto-grid of desk pods. Each top-level session gets a pod; its subagents and agy calls sit at the pod's extra desks ("interns"). Overflow goes to a second room. Agents walk in from the door (Arriving) and out (Leaving).
- Character animation per state: idle, typing, reading, running (glowing monitor), thinking (dots), needs-you (raised hand + blinking lamp), queued (waiting by the door), error (red bubble), done (arms up). Labels sit above heads, coloured by kind (Claude orange, sub amber, agy teal).
- Ambient characters (not agents; `hit_test` returns `office:cat` / `office:ceo`, selectable and hoverable):
  - The CEO (you), `render/ceo.rs`: a corner office reserved by `layout.rs` at the top-right (pods never overlap it), a big desk with a "CEO" nameplate. Original sprite in a red-and-black vertical-stripe jersey (no crest, logo or sponsor), black shorts, optional number on the back. Mood mirrors the office: any NeedsYou means it stands, points toward the alert and shows a "!" bubble; agents working means it reviews papers and nods; all idle means coffee, leaning back, or looking out (back view with the number).
  - The office cat, `render/cat.rs`: ~12×8 px procedural pet whose behaviour is a pure function of `t_ms` (no RNG state). It walks/hops between layout waypoints (sofa, windowsill, CEO desk, floor, desks), sits, grooms and sleeps (zZ), and sometimes hops onto the desk of a NeedsYou/Error agent. Drawn above furniture, below labels; its name shows only on hover/selection.
- Themes: `assets/themes/<name>/theme.toml` maps logical sprite names to (atlas file, x, y, w, h, frames). Themes are MIXABLE: config `theme = { floor = "cozy", furniture = "modern", characters = "retro" }`. A built-in `procedural` theme (drawn in code, no files) always works.

## TUI (crates/office/src/app/)
- The left area is the office. The right sidebar (toggle `s`) has: the agent list (label, role, model, state, elapsed, tokens, $), a router gauge (busy/capacity, free RAM, pressure), and an alerts feed. Footer: key hints.
- Keys: `↑/↓` select agent, `enter` details, `t` cycle theme / `T` theme mixer, `s` sidebar, `n` toggle notifications, `?` help, `q` quit.
- Control actions: `a`/`d` approve/deny the selected agent's pending Claude permission (only when office-hook --permission is installed; otherwise disabled with a hint); `x` cancel the selected agy call (SIGTERM the router pid after a y/n confirm); `o` resume the selected Claude session (copies `cd <cwd> && claude --resume <id>` to the clipboard via pbcopy and opens a new Ghostty window if available); `c` clear alerts.
- Notifications: Critical alerts and NeedsYou use `osascript -e 'display notification …'` (rate-limited, toggled with `n`).

## Alerts (office-core/alerts.rs)
loop (same tool + same summary 4× in a row), stuck (no events for 120 s while not Idle/Done/NeedsYou/Queued), tool failures in a row (3), needs_you (permission/AskUserQuestion), router tool_denied/timeout/busy/empty_response, and cost (session > config.cost_alert_usd).

## Config (~/.config/agent-office/config.toml)
theme mix, `[cat] enabled/name/color (grey|orange|black|white|calico)`, `[ceo] enabled/name/number/skin/hair/hair_style (short|curly|long|bald)`, fps (default 12), notifications on/off, cost_alert_usd, price table, the agy role→model map, permission_timeout_s (default 0 = off).

## Non-goals
No network access, no telemetry, and nothing outside the user's machine. Original name and art: we use only free (CC0/CC-BY) assets with attribution in `assets/CREDITS.md`.
