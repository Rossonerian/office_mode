# Vendored Kenney sheets: coordinate index

All tiles are 16x16. Cell `(col,row)` -> pixel origin: `x = col*pitch`, `y = row*pitch`, size 16x16.
Ranges like `(0..2,0)` mean cols 0 to 2 at row 0. Entries marked (?) are uncertain guesses from viewing the sheet.
Cell indices were read off gridded 5-9x enlargements, so they are exact; the names are visual guesses.

| Sheet | File | Size (px) | Grid | Tile | Gap/margin | pitch |
|---|---|---|---|---|---|---|
| Roguelike Indoors | `kenney-roguelike-indoors/roguelikeIndoor_transparent.png` | 458x305 | 27 x 18 | 16 | 1 px gap, 0 margin | 17 |
| 1-Bit Pack (colored) | `kenney-1-bit-pack/colored-transparent_packed.png` | 784x352 | 49 x 22 | 16 | none (packed) | 16 |
| Tiny Town | `kenney-tiny-town/tilemap_packed.png` | 192x176 | 12 x 11 | 16 | none (packed) | 16 |

## 1. Roguelike Indoors (pitch 17: x = col*17, y = row*17)

Furniture and props only. It has NO floors, walls, doors, windows, clocks, computers or outdoor tiles. Use Tiny Town
(floors/walls/doors/windows/roofs/trees) or solid colours for the room shell.

- Tables, wooden (3 cells wide = left/mid/right; top row, leg row below): rect (0..2,0)+(0..2,1); oval (3..4,0)+(3..4,1); narrow vertical (5,0)+(5,1); round (6,0)+(6,1), round top (7,0). More plain tables (4..7,2..5) (various leg styles), long benches/tables (4..7,6..8).
- Tables with tablecloth: orange (0..6,9..11), green (7..13,9..11): long (0..2,9)+(0..2,10), oval (3..4,9..10), vertical (5,9..10), round (6,9..10), small square (0,11), (4..6,11).
- Cloth runner overlays (table + runner, orange/green) (16..20,8..11) (?).
- Chairs: front-facing with cushion (0,2),(0,3),(0,7); back-facing (1,2..5); right-facing (2,2..8); left-facing (3,2..8). Plain wood (0..3,4..5), high back (0..1,6..7), grey/white (0..3,8). Top-down cushioned chairs (14..15,9..12) (?).
- Beds, side view: orange/green (8..9,0..3),(12..15,0..3) (14..15,0..3 green); top-down beds (8..13,6..8) orange/green/cream; plain headboards (8..9,4..5) tan, (12..13,4..5) cream.
- Sofas/benches: (4..7,6..8) long wooden benches (?); (10..11,3..5) short benches (?).
- Plants: potted plant (16,0),(17,0) (only plants in the sheet).
- Lamps: candelabra (19,0),(20,0), lit (19,1),(20,1); floor candle posts (19..20,2..5); wall candles (21..22,0..3); chandeliers (16..18,4..5); table candles (16..19,6); candelabras (16..19,7); wall torches (20..22,6..7).
- Shelves/bookcases: (19,17) low shelf with potions; (20,17),(21,17) bookcase with potions (last row; bottom may be partly cropped by sheet edge) (?).
- Kitchen counters/cabinets (grey top + wood front): rows 12..17, cols 0..9 (drawers (1,12), stove grating (4,12..17), jars (5..6,12..17), plates (7,12..17), sink (8,12),(8,14)); wood counters (8..10,15..16); fridge-like (11,15..16) (?).
- Desks / wide wooden tables: (23..26,10),(23..26,13..14),(23..26,17) plain brown (?); L-shaped orange (23..24,11..12) and green (23..24,15..16) (?).
- Wall decor: small frames (16..18,12..13); framed paintings/maps (19..21,12..14); wide banner paintings (16..18,14..17); portraits (19..21,15..16); mirrors (22,14..17).
- Rugs/carpets (?): orange (23..26,0..3), green (23..26,4..7); col 23 and 26 carry fringe/side trim.
- Dark grey: piano (23..24,8) + (23..24,9) (?); stoves (14..15,14..15), (12..13,17) (?).
- Misc: ladders (21,8..11), shields (22,8..11), clotheslines (16..18,1..2), pots (16..18,3), barrels/backpacks (21..22,4..5).

## 2. 1-Bit Pack colored-transparent_packed (pitch 16: x = col*16, y = row*16)

Limited palette (brown, cream, red, green, blue) on transparent. Cream people/monsters/weapons occupy cols 24..48, rows 0..10; icons/cards/letters cols 32..48 rows 11..21.

- Ground: rubble/dirt (1..4,0); grass tufts (5..7,0); maroon brick/floor (8..12,0..2),(16..17,0..1); water pools (8..12,4..5).
- Room shell (cream line style): corner/edge borders (16..21,13..21); diagonal wall corners (0..4,16..18); wall with arch (5,16..18); wall with door (6,16..18),(7,16..18); brick (9..13,17..18) (?).
- Doors: brown (3,9),(4,9),(5,9),(7,9),(8,9); blue (0..2,9..10); empty doorways (2,9),(6,9),(9,9) (?); cream (5..9,10).
- Windows (?): (3..4,11..14), (8..10,11..13).
- Desks/tables (brown): (2,7) desk, (4,7) desk with items, (6..8,7) tables, (9..10,7) drawer tables, (5,7) cabinet.
- Chairs/seats: (11,9) chair, (12,9) chair front, (14..15,9) sofa/bench (cream), (13,9) toilet (?), (12,10) toilet/sink (?), (13,10) bathtub (?).
- Bookshelves: (5,6) bookshelf, (3,7) bookcase, (0..1,6) rack/ladder, (3,6) chart board.
- Computers/screens (?): (2..3,8) monitor/TV frames, (4,8) long board, (16..17,10) brown monitor on stand, (14..15,8) wall boards with icon, (8,8) printer/fax and (9,8) typewriter (cream), (11..12,8) consoles/machines (cream), (10..11,6) cream machines/safes.
- Beds (?): (5..7,8) beds with checker blanket.
- Chests: (8,6),(9,6) brown chests.
- Plants/trees (green): trees (0..4,1), large tree (4,2), cactus (6..7,1), bush (5,1), crops (13..16,6), shrubs (0..3,2).
- Lamps/fire: candles (3..5,15); flames (14..15,10).
- Clock: none identified. Signposts (0..1,7),(14..16,7).
- Houses/roofs (?): red (0..1,19..20), cream/brown (2..8,19..21). No roads.

## 3. Tiny Town tilemap_packed (pitch 16: x = col*16, y = row*16)

Best sheet for outdoor/window-view scenery and room shells. Colourful with dark outlines.

- Grass: (0,0) plain, (1,0) tufts, (2,0) flowers.
- Dirt patch with grass border, 3x3: (0,1) TL, (1,1) T, (2,1) TR, (0,2) L, (1,2) centre, (2,2) R, (0,3) BL, (1,3) B, (2,3) BR. Dirt path strips (3..6,3); cobble patch on grass (7,3).
- Trees: orange (3,0..1) tall, (3,2) small; green (4,0..1) tall, (4,2) small; bush (5,0); sprouts (5,1); mushrooms (5,2). Forest clusters: green (6..8,0..2), orange (9..11,0..2) (overlapping multi-tile art).
- Roofs: blue-grey tile (0..2,4..5), with chimney+gable (3,4..5); red tile (4..6,4..5), with chimney+gable (7,4..5).
- House walls: brown timber (0..3,6) with door opening (2,6); stone-grey (4..7,6) with door opening (6,6).
- Windows/doors: window (0,7) brown, (4,7) grey; wooden doors (1,7),(2..3,7) brown, (5,7),(6..7,7) grey.
- Castle/stone: stone floor 3x3 (0..2,8..10); battlements (3..6,8); tower window (7,8); portcullis gate (3..4,9..10); arch gate (5..6,9); slit wall (5..7,10).
- Fences: pen 3x3 (8..10,3..5); horizontal (8..10,6); vertical (11,3..5); signpost (11,6); cart/stall (9,4) (?).
- Props: hay (9,7), beehive (10,7), target (11,7), well/bucket (8,7..8) (?), bomb (9,8), log (10,8), pot (11,8), tools/weapons (7..11,9..10), barrels (10..11,10).
- Missing: indoor floors/furniture, clocks, computers, lamps (take from Roguelike Indoors / 1-Bit).
