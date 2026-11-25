# Vendored Kenney sheets: coordinate index

All tiles are 16x16. Cell `(col,row)` -> pixel origin: `x = col*pitch`, `y = row*pitch`, size 16x16.
Ranges like `(0..2,0)` mean cols 0 to 2 at row 0. Entries marked (?) are uncertain guesses from viewing the sheet.
Cell indices were read off gridded 5-9x enlargements, so they are exact; the names are visual guesses.

| Sheet | File | Size (px) | Grid | Tile | Gap/margin | pitch |
|---|---|---|---|---|---|---|
| 1-Bit Pack (colored) | `kenney-1-bit-pack/colored-transparent_packed.png` | 784x352 | 49 x 22 | 16 | none (packed) | 16 |
| Tiny Town | `kenney-tiny-town/tilemap_packed.png` | 192x176 | 12 x 11 | 16 | none (packed) | 16 |


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
- Missing: indoor floors/furniture, clocks, computers, lamps (take from the 1-Bit pack).
