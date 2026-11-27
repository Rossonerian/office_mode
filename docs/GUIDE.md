# agent-office user guide

A short, friendly tour. Press `?` in the office any time for the key list.

## What you are looking at

The office is a tiny pixel-art building where your AI agents work.

- **Each Claude Code session gets its own coloured floor** (a "zone") with a nameplate on top. The
  main agent sits at the desk pod, its helper subagents sit at the extra desks.
- **agy calls** (Antigravity helpers started through the router) show up as interns and, while
  waiting for a free slot, queue by the door.
- **You are the CEO**, in the corner office at the top right. The office cat wanders the aisles.
- The **sidebar** (toggle with `s`) lists every agent, a router gauge (free RAM, slots) and the
  alerts feed.

An agent's pose tells you what it is doing: typing, reading, running a command, thinking, or
waiting for you. The small icon above its head says the same (`!` needs you, `?` question,
`…` thinking, `✓` done).

## Who is who (ROSSO titles)

If you use ROSSO, agents carry company titles instead of raw model names:

| Title | Job |
|---|---|
| Chief of Staff | your main Claude session: opens jobs, dispatches, arbitrates |
| Design Engineer | multi-file design and plans |
| Technician | makes the actual edits |
| Chief Inspector | reviews the changes |
| Test Inspector | runs checks and gives a PASS / FAIL verdict |
| Records Clerk | writes commit messages |
| Department Heads (`Backend Head`, `Frontend Head`, ...) | turn a plan into work orders for one area |
| Chief Architect and the Review Panel seats | the Strategy Council: propose and critique |

Council members sit together in the glass **council cabin**. Every agent has its own colour, and
each session its own floor colour, so you can follow who belongs where.

## Postmen, boss visits and votes

- A **postman** walks a **brief** (white envelope) from the sender's desk to the recipient, and
  a **report** (yellow folder) back. It means one agent just handed work to another.
- When a session finishes a turn, it **walks to your office** with a blue folder: that is a
  **result** for you. It shows up in your inbox.
- When a session asks you a question or presents a plan (AskUserQuestion / ExitPlanMode) it
  walks to you and waits. A **VOTE** ballot appears on your desk (approve or request changes)
  until you answer in the Claude session.

## Reading what agents say

- `r` opens **reports**: every brief, report and result. With an agent selected it shows that
  agent's; press `a` for all. `enter` opens one in full, `n` / `p` go to the next / previous.
- `i` opens your **inbox**: only things addressed to you. New ones also pop up as a small card
  at the bottom right (`enter` reads it, `esc` dismisses it) and add a `✉ N` badge in the header.
- The reader shows a one-line **status** (for example `✓ OK · Chief Architect (neon-xhigh) ·
  3 m 58 s`), and four views (Summary, Full, Brief, Raw; see below). Errors and
  questions for you are shown in red or yellow right under the title. Use `↑` `↓` / `PgUp`
  `PgDn` to scroll, `g` to go to the sender, `o` to resume its session, `esc` to go back.

### The report reader in detail

At the top of every report is a **header card**: a coloured verdict chip (green `PASS`, `APPROVE`,
`SUPPORT`, `OK`; red `FAIL`, `OBJECT`, `ERROR`, `TIMEOUT`, `BLOCKED`; yellow `INSUFFICIENT`,
`REVISE`; orange `CHANGES_REQUESTED`; magenta `NEEDS_USER_INPUT`), the ROSSO document type
(`Memo`, `Critique`, ...), whether it is a brief, report or result, the title, who sent it to
whom, how long ago, how long the call took and which model did it.

Below it are four **views**. Switch with `Tab` or `←` `→`:

| View | What it shows |
|---|---|
| Summary | the status line, the first paragraph, and short bullet cards for the lists it finds (Findings, Required changes, Risks, Open issues, Next steps, numbered items or the first table). Words like critical, major, minor, high or low colour the bullet. |
| Full | the whole text, formatted |
| Brief | what the agent was asked |
| Raw | the unformatted text, exactly as received |

File references such as `crates/x/y.rs:123` show in cyan and links are underlined. A scrollbar on
the right and an `n/N` counter in the title show where you are in the list. `y` copies the
current view as plain text to the clipboard, `w` saves the whole report as Markdown to
`~/Downloads/rosso-report-<number>.md` (a message at the bottom tells you the path).

## The sidebar tabs

The bar at the top of the sidebar has four tabs. Press `1` to `4` or click one (the theme picker
keeps its own `1`-`9` while it is open). With the sidebar hidden, pressing a number brings it back.

| Tab | What it shows |
|---|---|
| `1 Agents` | the agent list, the selected agent's inspector, the router gauge and the alerts (the original view) |
| `2 Jobs` | one block per ROSSO job: its name, the subject, how many documents it has, who sent the last one, a status (`active`, `idle`, `done`, `blocked`, `failed`), the latest verdict, how long ago, and a progress bar. The bar shows how far the job got through the pipeline: memo, task card, critique, decision, work order, sign-off, inspection. `↑` `↓` pick a job, `enter` opens its latest document. |
| `3 Activity` | a timeline, newest first: `✉` a brief, `📁` a report, `⬆` a result for you, `⚠` an escalation, `✓` / `✗` a passed / failed verdict, `●` an agent that needs you, hit an error or finished. `↑` `↓` pick a line, `enter` reads the document. |
| `4 Stats` | the router's busy slots against its capacity (green, then yellow, then red as it fills), memory pressure and queue, alert counts by level, token totals, a small graph of total cost over the last 30 minutes, another of how many agents were active, and bars for what each session has cost (top six). The graphs fill in as time passes: a point is taken every 10 seconds. |

The header also shows a compact strip on the right when the window is wide enough:
`● 3 active · ✉ 2 · $4.12 · router 2/5` (agents working, unread inbox items, total cost, busy and
total router slots).

## Native Desktop App (`office desktop` / `office-desktop`)

In addition to the terminal TUI (`office` / `office_mode`), `office_mode` ships a standalone native desktop GUI application (`office-desktop`, built with `eframe`/`egui`) for macOS, Linux, and Windows:

```sh
office desktop          # launch live native desktop office
office desktop --demo   # preview with synthetic agents
```

You can also run `office-desktop` directly. For `office desktop` to launch it safely, `office-desktop` must be installed in the same directory as `office` (Homebrew, Scoop, and the install scripts do this automatically).

- **Shared live state:** Both the desktop app and the terminal TUI read the same `~/.config/agent-office/config.toml`, themes, CEO outfits, whiteboard checklist, and live hook socket.
- **Direct interaction:** Click agents, sidebar tabs, and the wall whiteboard directly in a crisp pixel-art window without needing Kitty Graphics or Sixel terminal support.
- **Drag-and-drop asset import:** Drag a theme folder, `.png` sprite or CEO outfit, or `.wav` chime directly onto the desktop window to validate and import it into `~/.config/agent-office/`.

## Office Whiteboard Checklist & Progress Tracker

The office wall includes an interactive **Whiteboard** that tracks your checklist and displays a live completion progress bar both on the pixel-art wall and inside the popup.

- **Open it:** Press `b`, click the wall whiteboard directly, or pick `b` from the Command Center palette (`Space`).
- **Manage tasks:**
  - `a` — add a new manual task (`Enter` saves, `Esc` cancels)
  - `Space` or `Enter` — toggle the selected task done / not done
  - `d` / `Delete` / `Backspace` — remove the selected task
  - `C` — clear all completed manual tasks
  - `w` — toggle automatic **ROSSO wave sync** (`sync_waves`), which mirrors active pipeline wave items onto the board alongside your manual tasks
  - `Esc` / `b` / `q` — close the whiteboard
- **Persistence:** Saved atomically to `~/.config/agent-office/whiteboard.json` and checked by `office doctor`.

## Keys

| Key | What it does |
|---|---|
| `↑ ↓` / `j k`, `g G` | pick an agent (or click one), first / last |
| `Tab` | jump to the next agent that needs attention |
| `enter` | agent details (scroll with `PgUp` / `PgDn`) |
| `1` `2` `3` `4` | sidebar tabs: Agents, Jobs, Activity, Stats |
| `r` / `i` | reports / inbox |
| `Tab` `←` `→`, `y`, `w` | inside a report: change view, copy the view, save as Markdown |
| `b` | open the Whiteboard checklist (or click the wall whiteboard) |
| `Space` | Command Center quick-actions palette (`p`, `f`, `K`, `R`, `A`, `i`, `o`, `B`, `b`, `O`) |
| `p` / `f` | dispatch a new task (`agy` / `claude` / `rosso-auto`) / follow-up reply to selected agent |
| `a` / `d` / `A` | approve / deny pending permission, or `A` to approve all pending |
| `x` | stop / cancel the selected agent or router call (asks `y`/`n`) |
| `K` | Free RAM dialog: stop idle agents or orphan `agy` processes |
| `R` / `X` *(Jobs tab)* | resume / abort the selected `rosso-auto` job |
| `o` | resume the selected session in a terminal |
| `v` | View menu: choose what the office shows (`c` clean, `a` all, `h` hide) |
| `z` | toggle fill-viewport vs pixel-exact scaling |
| `L` | show / hide name tags and speech bubbles |
| `t`, `[` `]`, `T` | theme picker, previous / next theme, theme mixer |
| `Space` → `O` | CEO Outfit picker (`(procedural)` or imported outfits) |
| `s` | show / hide the sidebar (persisted) |
| `n` | desktop notifications on / off |
| `m` | cross-platform sound chimes on / off |
| `c` / `C` | clear alerts / bring them back |
| `?` | help |
| `q` / `ctrl-c` | quit (agents keep working) |

## The View menu (`v`)

Move with `↑` `↓`, toggle with `space`, `c` applies the clean preset (labels on selected/alerting agents only), `a` shows everything, `h` hides the overlays, `esc` saves.
The highlighted row is explained at the bottom.

| Option | What it is |
|---|---|
| names | the name tag above each agent |
| bubbles | what each agent is doing right now (speech bubble) |
| icons | small `!`, `?`, `…` and `✓` above heads |
| nameplates | the session name on each coloured floor |
| zones | the coloured floor under each session |
| couriers | postmen carrying briefs and reports |
| boss visits | sessions walking to your office with results or plans |
| cat | the office cat |
| CEO corner | your corner office |
| council cabin | meeting room for council members |
| ambient | coffee steam, server lights, kanban, idle walks |
| selected only | show tags and bubbles only for the selected agent and for alerts |
| ambient lighting & glow | monitor glow and subtle room lighting |
| fill viewport | scale the office to fill the pane (`z` shortcut) |
| CRT scanlines | optional subtle retro scanline overlay |
| theme-matched TUI chrome | tint borders and accents to match the active theme |

`L` is a shortcut for names + bubbles together ("name tags + bubbles").

## Themes, Sprites & CEO Outfit Import

`t` opens the picker (arrow keys preview live, `enter` keeps, `esc` undoes). `[` and `]` step
through presets. `T` lets you pick a different theme for the floor, walls, furniture, decor and
characters. Your choice is saved.

### Importing custom themes, sprites, and CEO outfits

Use `office import` (or drag and drop files onto the `office-desktop` window) to install custom
validated assets into `~/.config/agent-office/`:

```sh
office import theme <dir> <name>                                # -> ~/.config/agent-office/themes/<name>/
office import sprite <theme> <sprite> <png>                     # override one sprite in a theme
office import ceo-outfit <png-or-dir> <name> [--mode torso|full] # -> ~/.config/agent-office/outfits/<name>/
```

- Supported sprite names for `office import sprite`: `floor`, `wall`, `window`, `door`, `desk`,
  `monitor_off`, `monitor_on`, `chair`, `plant`, `sofa`, `cooler`, `whiteboard`, `lamp`, `rug`.
- Pack names must match `^[a-z0-9][a-z0-9_-]{0,47}$`; PNGs are validated before copying.

### Switching your CEO Outfit (`Space` → `O`)

Open the Command Center palette with `Space` and press `O` to open the **CEO Outfit** picker:

- Pick `(procedural)` or any installed outfit pack from `~/.config/agent-office/outfits/`.
- Press `Tab` (or `t`) to switch between **`torso`** mode (overlay on the CEO's jersey/shirt) and **`full`** mode (full character outfit sprite).
- Press `r` to reload themes and newly imported outfits from disk without restarting.
- Press `Enter` to apply and persist `outfit` / `outfit_mode` to `[ceo]` in `~/.config/agent-office/config.toml` (where you can also set `primary_color` and `secondary_color` hex overrides).

## Sound and notifications

- `n` toggles **desktop notifications** (agent needs you, critical alerts, new inbox items).
- `m` toggles **cross-platform sound chimes**. Turning it on plays a short test chime so you know
  it works (or shows a warning toast if no playback backend is available). At most one chime plays
  every 3 seconds.
- **Platforms & backends:**
  - **macOS:** `afplay` — uses system `.aiff` chimes (`Pop` = test, `Glass` = agent needs you, `Ping` = inbox item, `Basso` = error/escalation) when `mode = "system"`, or synthesized WAVs when `mode = "synth"`.
  - **Linux:** auto-detects `paplay`, `pw-play`, `aplay`, or `canberra-gtk-play` and plays built-in pure-Rust synthesized WAV chimes.
  - **Windows:** plays synthesized or custom WAV chimes directly in memory via Win32 `PlaySoundW`.
- **Custom WAV slots:** Import your own `.wav` files (up to 4 MiB, validated RIFF/WAVE) with:

  ```sh
  office import sound <test|needs-you|inbox|error> <wav>
  ```

  Files are stored under `~/.config/agent-office/sounds/` and configured under `[sounds]` in `~/.config/agent-office/config.toml`:

  ```toml
  sound = true

  [sounds]
  mode = "system"          # "system" | "synth"
  # test = "test.wav"
  # needs_you = "needs-you.wav"
  # inbox = "inbox.wav"
  # error = "error.wav"
  ```

- Both `notifications` and `sound` are saved in `~/.config/agent-office/config.toml` and work independently of each other.

## Troubleshooting

Run `office doctor`. It checks the config, hooks, socket, themes, `office-desktop`, audio backend,
custom WAVs, CEO outfits, whiteboard JSON, `agy-route` router, and transcripts, and tells you what
to fix. Common cases:

- **Nothing appears**: run `office install-hooks`, then restart the Claude session.
- **`office desktop` says `office-desktop not installed next to office`**: ensure `office-desktop` is installed in the same bin directory as `office` (re-run the installer, `brew upgrade office_mode`, or `cargo install --path crates/office-desktop`).
- **No sound**: press `m` and listen for the test chime, then run `office doctor` to inspect the detected audio backend. On macOS, check `afplay /System/Library/Sounds/Glass.aiff`; on Linux, ensure one of `paplay`, `pw-play`, `aplay`, or `canberra-gtk-play` is in `PATH`.
- **Approve / deny does nothing**: set `permission_timeout_s` above 0 in the config and run
  `office install-hooks --permission`.
- **Blocky picture in the terminal**: your terminal has no image support, so the office is drawn in half-blocks.
  Use Ghostty, kitty, or WezTerm — or launch the native desktop app with `office desktop`.
- `office config` prints the settings that are actually in effect; `office demo` (or `office desktop --demo`) runs with fake
  agents so you can try every key safely.
