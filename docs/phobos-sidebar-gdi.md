# Optional Phobos sidebar positions

Build with `--features phobos` to choose the hardcoded sidebar coordinate
table per side, independently of the SIDE MIX artwork.

```ini
[SOMESIDE]                    ; side section in rulesmd.ini
Sidebar.GDIPositions=         ; boolean
```

The first side in `[Sides]` defaults to `yes` (GDI positions); every other
side defaults to `no` (NOD positions), matching Phobos `SideExt`'s constructor
`ArrayIndex == 0`. The key changes the tab, cameo, repair/sell, top-button,
scroll, power-bar and radar coordinates only; the side's SIDE MIX art and
chrome heights still follow the side theme.

Source: `SideExt::Sidebar_GDIPositions` and `SideExt::Initialize`
(`src/Ext/Side/Body.cpp`); hooks `0x534FA7` (`Prep_For_Side`), `0x652EAB`
(radar), `0x6A5090`/`0x6A51E9` (sidebar positions/GUI) and `0x63FB5D`/`0x6403DF`
(power bar) in `src/Ext/Side/Hooks.SidebarGDIPositions.cpp`.

## The radar

`RadarClass::Init_For_House @ 0x00652E90` reads the same flag and branches at
`0x00652EB9`, and the two arms differ in exactly four constants:

| | radar gadget x | radar gadget y |
| --- | --- | --- |
| GDI (`0x00652EDB`, `0x00652EE9`) | base + `0xB` | base + `4` |
| NOD (`0x00652F1F`, `0x00652F2D`) | base + `0xE` | base + `5` |

Everything else is shared: the aperture is the loaded radar source frame
(`0x00652F85` reads width and height out of the `0xB048E0` frame header, stock
140x108), and the second overlay gadget at `0xB048C8`/`0xB048CC` is that same
base plus a fixed `0x48` in x. So the NOD aperture sits **3 right and 1 down**
from the GDI one. The spec carries the resulting position as `radar_x`/
`radar_y`; the aperture size stays the source frame.

## Where VERA20K owns it

- `rules/ruleset.rs` — `side_gdi_positions`, read from each side's section in
  `[Sides]` order, with the first-side default and configuration hash.
- `ui/sidebar/layout_spec.rs` — `for_theme_and_positions`, which keeps the art
  theme separate from the GDI/NOD coordinate choice and carries the radar
  offset.
- `ui/sidebar/mod.rs` — `radar_minimap_rect_with_spec`, the aperture every
  consumer shares (draw, minimap input region, tactical capture crop).
- `app/presentation/sidebar_render.rs` — resolves the active side through the
  existing theme/side owner and installs the chosen layout.

## Residuals

- The radar aperture's absolute base is the verified GDI position
  (`sidebar left + 16`, top 49); the NOD arm applies the native delta to it
  rather than re-deriving the base from `RadarClass+0x11E4`, whose writers are
  the constructor's zero and a virtual this port does not resolve.
- The second overlay gadget at `0xB048C8`/`0xB048CC` has no VERA counterpart.
- The style follows the configured side; changing sides mid-match rebuilds the
  sidebar on the existing theme transition.
