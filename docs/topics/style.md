# Style tokens

A model carries a **style**: design tokens items read instead of literal
values, so a generated scene has one cohesive look and restyles in one call
(`src/style.rs`). Set it first, then build with tokens.

- `style(action, tokens?, name?, ...)`: `show` (tokens; `image: true` adds a swatch picture),
  `set` (merge `tokens`; a null removes a token), `replace`, `clear`, and
  the library (`<data>/styles`): `list`, `describe`, `save` (name,
  description, tags, prompt), `load` (becomes the model's style), `delete`.
- Tokens: `brief` (the feel in words, shown in `scene_summary`),
  `colors` (`"@primary"` anywhere a colour goes, with `.darken(x)`,
  `.lighten(x)`, `.mix(@b, x)`, `.alpha(x)`, `.saturate(x)`,
  `.desaturate(x)`; colours may alias `@others`), `materials` (presets, used
  as `{"type": "material", "name": "top", "style": "plastic"}`; the item's own
  fields win), `numbers` (`"$bevel"`, like params; a model param of the same
  name wins), `defaults` (per item type, fields filled in when omitted:
  `{"round": {"radius": "$bevel"}, "revolve": {"segments": "$segments"}}`).
- There are no built-in styles: each project defines its own. A worked
  example (a pastel toy look), then an item that uses it:

  ```json
  {"name": "style", "args": {"action": "set", "tokens": {
    "brief": "chunky toy: pastels, soft rounded edges, low-poly curves",
    "colors": {"primary": "#e07a5f", "secondary": "#81b29a", "wall": "#f4e1d2",
               "ink": "#3d405b", "trim": "@primary.darken(0.25)"},
    "materials": {"plastic": {"color": "@primary", "roughness": 0.45},
                  "metal": {"color": "@ink.lighten(0.3)", "roughness": 0.35, "metalness": 1}},
    "numbers": {"bevel": 0.04, "segments": 10},
    "defaults": {"round": {"radius": "$bevel"}, "revolve": {"segments": "$segments"}}}}}
  ```

  `{"type": "material", "name": "table_top", "style": "plastic"}`,
  `{"type": "material", "name": "ball", "color": "@secondary"}`,
  `{"type": "round", "name": "top", "input": "top_raw", "mode": "fillet"}`
  (radius from `$bevel`). Then `style set` with new colours or `style load`
  of another saved style restyles the whole scene with no op edits.
- Items that read the style keep their unresolved form (a template, like
  params): `style set` / `load` re-derives every one whose resolved form
  changed, as one history step (undo restores the old style). Items applied
  before a style existed have no template and don't follow later styles.
