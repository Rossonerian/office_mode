# Asset credits

All bundled sprite sheets are by **Kenney (kenney.nl), CC0 1.0** (public domain dedication,
https://creativecommons.org/publicdomain/zero/1.0/). Credit is not required but given gladly.

| Pack | Source | Used in | File(s) |
|---|---|---|---|
| Roguelike Indoors | https://kenney.nl/assets/roguelike-indoors | `modern`, `cozy` | `roguelikeIndoor.png` |
| Roguelike Modern City | https://kenney.nl/assets/roguelike-modern-city | `modern` | `roguelikeCity.png` (magenta key made transparent) |
| 1-Bit Pack | https://kenney.nl/assets/1-bit-pack | `retro`, `cyberpunk` | `onebit_monochrome.png` |

The zips were fetched from Kenney's own uploads of the same packs on OpenGameArt
(https://opengameart.org/content/roguelike-indoor-pack, .../roguelike-modern-city-pack, .../1-bit-pack),
each marked CC0 there and on kenney.nl.

The `procedural` theme, all characters, the 3×5 font and every fallback sprite are original and drawn in code
(`crates/office/src/render/`). No assets from Pixel Agents, Pixtuoid, LimeZu or LPC are used.

## Vendored sheets under `assets/vendor/` (from kenney.nl)

All by **Kenney (Kenney Vleugels, kenney.nl)**, licence **Creative Commons Zero (CC0 1.0)**. Each folder keeps the
pack's own `LICENSE.txt` with the source URL appended. Coordinates are catalogued in `vendor/INDEX.md`.

### Roguelike Indoors (`vendor/kenney-roguelike-indoors/`)
- Source: https://kenney.nl/assets/roguelike-indoors, author Kenney, CC0.
- Files kept: `roguelikeIndoor_transparent.png` (458x305, 16 px tiles, 1 px gap), `LICENSE.txt`.

### 1-Bit Pack (`vendor/kenney-1-bit-pack/`)
- Source: https://kenney.nl/assets/1-bit-pack, author Kenney, CC0.
- Files kept: `colored-transparent_packed.png` (784x352, 16 px tiles, no gap), `LICENSE.txt`.

### Tiny Town (`vendor/kenney-tiny-town/`)
- Source: https://kenney.nl/assets/tiny-town, author Kenney, CC0.
- Files kept: `tilemap_packed.png` (192x176, 16 px tiles, no gap), `LICENSE.txt`.

### Which theme uses which Kenney pack
| Theme | Packs |
|---|---|
| `kenney-cozy` | Roguelike Indoors (plant, chair, sofa, shelf, candle lamp, desk) + Tiny Town (window view) |
| `retro-1bit`, `retro-amber`, `retro-gameboy` | 1-Bit Pack coloured sheet (door, chair, sofa, plant, shelf), palette-remapped, + Tiny Town (window view) |
| `cozy`, `midnight` | Tiny Town (window view; `cozy` also Roguelike Indoors as before) |
Theme atlases reference the vendored sheets via `../../vendor/<pack>/<sheet>.png`.
