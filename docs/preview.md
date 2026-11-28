# Preview window (human side)

Left-drag = orbit, middle-drag = pan (pan follows the cursor, with a floor so
large scenes stay quick to pan when zoomed in), right-drag = look around
from where the camera is, and while it is held WASD fly along the view, Q / E
down / up, Shift 4x (speed scales with the scene; S doesn't toggle the style
pane then), scroll = zoom (trackpad pixel
scroll is scaled to wheel lines), arrows + PageUp/Down too.
Auto-drift stops once the human steers; a new model re-frames the camera
around true world bounds (bounds include part transforms), while edits to
the same model keep a camera the human steered.

**Idle costs nothing** (`grid::update_mode`): the window draws every frame
only while something changes (the document, the camera, input, playback,
a typed comment, agent shots or screenshots waiting, streaming jobs) and
for 2 s after (`SETTLE`, so TAA and SSR settle); then it switches to
winit's low-power reactive mode and draws nothing until window input, a
server call (`install_wake` wakes the loop on every command) or 1 s
(`IDLE_WAIT`, for the pane clocks). The auto-drift turns the camera for
20 s after each change to the shown document (`DRIFT_FOR`), not forever:
it used to keep an untouched window drawing at full rate, 55-65% of a core
with one empty document open. Offscreen and headless servers have no
winit loop, so agent renders are unaffected.

**Design system** (`preview/ds.rs`): every pane builds from its tokens
(IBM Plex Sans and Mono from `fonts/`, loaded once as `ds::Face`; the concept's colours `BG` / `PANEL` / `RAISE` / `LINE` / `FG` / `DIM` /
`FAINT` / `ACCENT`, type scale `ds::Type`, `ds::theme()` retuning Feathers'
widgets to the same palette), its line icons (`ds::Icon`, rasterized once,
tinted) and its pieces (`ds::piece(Kind::Tab | Row | Chip | Button |
IconButton ...)` + `ds::Ui { on, disabled }`; `ds::paint` restyles fill,
border and `ds::Ink` children from that state and `Hovered`). New UI uses
these instead of literal colours or Feathers buttons. Never give a UI node
a margin that can exceed its parent's width (rows in a list: pad the list):
Bevy's border-radius clamp panics on a negative size.

Layout (`preview/workbench.rs`, after `design/workbench-concept.html`):
a top bar, then either **All documents** (the grid, on by default;
`MODELER_GRID=0` starts without, never offscreen) or **one document in a
mode**: Model, Material, Animate, Map (keys 1-4; a mode the document can't
use is disabled, `mode_available`; a document opens in Map when it has
terrain, else Model). `Workbench` holds the mode, selection, hover, open
comments and shading; every pane spawns under a `PaneSlot` root (`InSlot`)
that `apply_layout` sizes each frame: top bar 48 px, left 250, right 290
(only with a selection, open comments, or in Material), the Animate dock
210 under the centre, and the centre, where the 3D camera renders to an
image of exactly that size (`View3d`), so `screenshot` never includes a
pane and screen-space reflections line up. In Material the 3D camera is
off and the graph fills the centre. `ui_hover` (cursor outside the centre
or over a `BlocksCamera` node) keeps the camera and picking off the mouse.
Keys stay off while a text field has focus or a comment is being typed.
Esc deselects, C toggles comments, G All documents.

- **3D view**: hovering a part glows it, a click selects it (empty space
  deselects); while comments are open a click pins that spot and opens the
  comment box (`picking.rs`, the glow in `panel.rs`). Pins for existing
  comments show while comments are open and come back after rebuilds.
- **Right pane** (`right_pane.rs`): the selection's properties (what made
  it, material, triangles, size; a marker's kind, preview, components) and
  its comments with "Add a comment", or, with nothing selected, every
  comment numbered like its pin (click selects the part, × deletes).
  Right-clicking a left-pane row or a material node comments on that; a
  right-click on a part in the 3D view pins a comment on that spot any time.
  The **comment box** (`comments.rs`) floats at the 3D view's top right,
  outside every pane, so pane rebuilds (any agent edit) never touch it,
  its focus or its draft: title + ×, a wrapping input with focus, Cancel /
  Post. Enter posts, Shift+Enter is a new line, Esc / × / Cancel discard,
  a click on empty space closes an empty box (typed text stays), clicking
  another spot moves the pin and keeps the draft, closing comments or
  switching documents cancels. A spot box keeps its own copy of the pin
  (the session's `pending_pin` is dropped on every model change and agents
  may consume it). While it is open the loop runs continuously and arrows /
  Page keys edit text instead of orbiting. **Pins** (`picking.rs`) are UI
  dots (16 px, numbered; `+` for the open box's spot) rebuilt only when the
  comments change (`Pins`, keyed by doc + `annotation_rev`), and placed by
  `place_pins` each frame from the camera's own `Transform` after
  `orbit_camera` / `apply_layout`, so they track the 3D point in the same
  frame. A dot's text label shows only on hover; while the camera moves
  dots dim and labels hide. `set_camera(ui: {comment: {part, spot?, text?,
  action?: post / cancel}})` drives the box for checking the UI. The app icon is `icons/app-icon.png` (drawn from `icons/app-icon.svg`; `preview/app_icon.rs` sets the Dock icon on macOS, the window icon elsewhere).
  The comments chip ends the 3D view's HUD row.
- **Streaming** (`preview/stream.rs`): the window and grid tiles never
  convert meshes or bake materials on the main thread. Part -> Mesh
  (content hash, tangents, closed check), material bakes (with mips) and
  decimated copies run on the `AsyncComputeTaskPool`; `try_bundle` answers
  from `PbrCache` / `Streams` and queues what is missing, `poll_streams`
  lands finished jobs. Until a changed part is ready the window keeps its
  old entity (or shows the mesh in its flat colour while the bake runs; a
  part whose procedural bake is pending or failed draws matte, not as a
  white mirror, and bake errors go to `daemon.log`).
  Parts over 50k triangles get two `simplify` LODs (1/4 from 6 part radii,
  1/16 from 20) as `PartLod` children swapped by `VisibilityRange` (window
  only: an agent shot drops the ranges and hides the copies for its frame).
  Tiles draw proxies (at most 20k triangles a part, ~100k a tile), spawn
  at most 300 parts a frame across tiles, derive ops-only documents off
  thread, and keep their last image until the new render is complete.
  `screenshot` waits until nothing streams (`SceneBuilt.pending`,
  `Streams.pending_grid`) plus 12 frames; agent views build their
  stand-ins synchronously (`part_bundle`), so both stay complete.
- **All documents** tiles are cheap by design: each is its own camera on
  its own render layer rendering into its own image, active only for a few
  frames after its document changes, so an idle grid renders no 3D; the
  window camera pauses while it is up, and a `screenshot` then captures the
  whole window (top bar, tiles, feed). Click a tile to open that document.

Material mode (`preview/material_pane.rs`): the graph fills the centre
on a dotted background, one-row node cards (28 px thumbnail, name, type in
mono) in columns by dependency level, curved edges; one `UiTransform` on a
zero-size canvas holds pan + zoom (wheel zooms about the cursor,
middle-drag pans, the view fits on a new material or pane size). Click
selects a node, right-click comments on it (empty graph: the material).
The right slot gets its own root: a lit sphere of the material (or the
selected node's output), base colour / roughness / normal / height
thumbnails from the bake, then the node's fields (or the material's
domain, tile and params).

- **Top bar** (`preview/top_bar.rs`): [All documents (N open)] [project / document ▾], the mode buttons 1-4 (unavailable ones dimmed with the `mode_available` reason as a tooltip), and one pill per agent (`Session::clients()`) with a green dot while its document changed in the last 4 s (`last_touched`). It is rebuilt only when a hash of what it shows changes. The document half or ⌘K / Ctrl+K opens the **document popup** (`preview/doc_popup.rs`): a search box, the open documents grouped by project (modes, last author, time ago, unsaved), and per project "Open from library…" listing matching catalog prefabs once something is typed. A click or Enter (the first row) switches or opens; Esc closes.

- **Animate dock, View menu, Map navigation** (`timeline.rs`, `shading.rs`,
  `camera.rs`, `minimap.rs`): Animate shows a dock under the 3D view (Play /
  Pause, the clip (click cycles), a ruler to drag-scrub, one lane per bone
  with diamond keys, hollow for mirrored tracks, a line for wave / sway
  motion, a playhead; Space plays) and draws the bones as window-only gizmos
  (render layer 30, which only the window camera adds). `HudRow` is the
  chip row at the 3D view's top right: the View menu (Lit / Clay / By part
  swaps `MeshMaterial3d`, restoring each part's `LitMaterial`) and, in Map,
  Fly / Orbit (F; fly = left-drag looks, WASD always moves). Map's minimap
  (bottom right) renders only for a few frames after the model changes;
  click or drag it to move the camera target.

- **Left pane** (`preview/left_pane.rs`) follows the mode: Model = the part
  tree with a type badge per row; Material = this document's materials
  (swatch, parts), then the library (a click makes one active); Animate =
  clips (a click plays one), then bones as a tree; Map = layer eye toggles
  (`LayerVisibility`, view only, read by `app.rs` `rebuild_scene` /
  `draw_game_gizmos`), then instances and markers (a click selects and
  frames). It rebuilds only when its content changes; selection restyles
  rows in place.
- **All documents** (`preview/grid.rs`): a sticky filter bar (text, All /
  Agent working / Unsaved), sections per project, scrolling with the wheel
  and Page Up / Down / Home / End, and only on-screen tiles exist (camera
  and parts, at most 24). Each tile has a Feathers × button and asks inside
  the tile before closing unsaved work. The right column is an agent feed
  of authored history steps across open documents (a click opens one).
