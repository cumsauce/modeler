# agent_views, screenshot, set_camera

- `agent_views(views, width?, height?, focus?, grid?, lighting?)`: offscreen renders,
  independent of the human's window.
  - Up to 9 views: `front`/`back`/`left`/`right`/`top`/`bottom`
    (orthographic, auto-framed), `iso` (perspective), or a camera you place,
    `{yaw, pitch, dist?, target?, ortho?, fov?}` (yaw 0 = from +z/front,
    90 = from +x/right).
  - Several views come back tiled into ONE labelled image (`grid: false` for
    separate images), so `["front", "right", "top", "iso"]` is one read.
  - The top view is a plan: +x right, -z up. Orthographic top / bottom
    views frame at least half the model's largest extent, so a tall
    model's plan is not a close-up.
  - `focus: [parts]` frames just those (`*` globs work: `"bike.*"`), and framing works at any scale.
  - A shadowed key light makes openings read even in orthographic views.
  - Both the window and agent cameras are lit by a procedural photo
    studio (`src/material/env.rs`: soft boxes over a grey cyclorama, filtered
    on the GPU as an environment light), so glossy and metal parts show real
    reflections. The window camera adds screen-space reflections (deferred
    rendering, MSAA off, TAA averaging SSR's per-frame noise) so glossy
    parts also mirror the parts around them. Agent views have no SSR: a
    single-frame shot kept its noise as speckle on glossy/metal parts and
    its smears read as geometry. They are deferred too, single-frame with
    SMAA, so shots are deterministic (an unchanged view `diff`s to 0%).
    Agent views add SSAO contact shading in perspective views only (Bevy's SSAO
    mis-handles orthographic cameras and draws concentric contour rings on
    smooth surfaces, so ortho shots go without; the shadowed key still gives
    them depth); the window does not (SSAO froze
    its presentation on Metal, and so does a forward window camera over the
    deferred materials: keep the window camera deferred).
    `material_view` shades its sphere with the same studio. Renders wait for
    the environment light to finish filtering, so a shot right after startup
    is never blank.
  - **Image cost** is pixels and image count (about w x h / 750 tokens per
    image; JPEG/WebP would not change it). So: a call's images total at
    most ~1.15 MP (long edge 1568); `detail: "low"` (the default) gives a
    grid cell about 448x336 worth of pixels (a single view 896x504), and
    `"high"` the old 640x480 / 1280x720. With no `width` / `height` the
    canvas takes the model's own proportions (`natural_aspect`: a standing
    figure gets a tall cell, a table a wide one), and the framing is tight:
    the cardinal views keep 8% slack, `iso` and custom perspective views fit
    the box's corners to the frustum (`fit_perspective`), not its bounding
    sphere. Every image result ends with `~N image tokens`.
  - `style: "ids"`: each part a flat, unlit, distinct colour with a legend
    (and the colour order in the text), to tell which shape is which part.
  - `style: "curvature"`: each part painted by mean curvature (white flat,
    red convex, blue concave, saturated where the radius of curvature is a
    sixteenth of the part or tighter): lumps, pinches, dents and creases on
    organic surfaces stand out where shading hides them.
  - `style: "silhouette"`: flat black parts on white, no shading, no
    decals: compare outlines and proportions at a glance.
  - `style: "density"`: each surface tinted by how much geometry is near
    it (vertices and distinct parts within a 24th of the scene): red =
    sparse (a bare wall, an empty floor), white, green = busy. Finds the
    empty stretches of a level that need dressing.
  - `focus: "joints"` (or `joints: N`, 1..9; "joints" = 4): close-ups of
    up to N places where visible parts meet, deepest overlaps first, then
    touching pairs with the tightest contact patch, each framed on the
    contact region (padded to show both parts) and labelled `a x b overlap
    12.0 mm` / `a x b touch`. Seen from the first view (iso by default);
    combines with any style. A quick look at every seam a model has.
  - `reference: {"image": "refs/chair_front.png", "view": "front"}`:
    renders the silhouette in that orthographic view and compares it with
    a reference PNG inside the call's project (resolved like `ops_file`;
    any PNG colour type). The image's subject is its opaque pixels when it
    has transparency, else the pixels contrasting with its border; it is
    fitted to the model's silhouette box (same height, same centre; by
    width when it is much wider than tall). Returns the silhouette and an
    overlay (magenta = model only, cyan = reference only, grey = both) and
    the IoU, and says when the aspect ratios differ. It also splits the
    fitted silhouettes into 5 horizontal bands (top / upper / middle /
    lower / bottom, comparing mean widths) and 5 vertical bands (left /
    center-left / center / center-right / right, comparing heights) and
    names the biggest 5 deviations of 3% or more, in meters and %
    (`front: top 0-20%: model 14% wider (+3.1 cm)`); structured clients
    get every band as JSON (`references[].bands`, `deviations`).
    `reference` may be a list of `{image, view}` (front, side and top
    references in one call, at most 4), each a silhouette + overlay pair
    in one grid.
  - `dims: true`: the framed box's width and height and a scale bar, at true
    scale in orthographic views. Perspective views have no single scale:
    they state the model's box size ("not the view") and a scale bar valid
    only at the look-at point's depth.
  - `diff: true` (cropped to what changed, one image per view; none when
    nothing changed): compares with the previous render of the same view, size,
    focus and style: magenta where something now is, cyan where something
    was, the rest greyed, plus the changed share and pixel box.

- `screenshot` (your document from the agents' own camera, auto-fitted;
  aim it with `set_camera(yaw, pitch, dist)`, same yaw convention as agent
  cameras. Agents never move the human's view: the window stays on the
  document the human picked (`Session.viewer_doc`; agent calls switch to
  their own document and back at `end_call`, and the window skips
  rebuilding while a call is away, `viewer_away`), and set_camera aims
  only the agents' camera. With `--offscreen` there is no human and it is
  the one orbit camera).
  Render tools time out after 15 s instead of hanging. **Always look at an
  image after building**: the data can be right while the render is wrong.
- `set_camera(ui: {mode, select, comments, shading, popup, capture, comment})` drives the
  window's workbench for checking the UI: `mode` model / material /
  animate / map / grid, `select` a part (`""` clears), `comments` open or
  closed, `popup` the document popup open or closed, `shading` lit / clay / parts, and `capture: "window"` makes
  `screenshot` capture the whole window (panes included) instead of the 3D
  view.
