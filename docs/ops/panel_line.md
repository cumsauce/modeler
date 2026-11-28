# `panel_line`

- **`panel_line`** draws a line in an orthographic `view` and cuts it into
  the first surface seen from there, as a groove `width` wide and `depth`
  deep, square to the surface at every point: shut lines round doors,
  hoods and bumpers, panel seams, trim grooves. `points` are in the view's
  own axes, meters: `right` / `left` `[z, y]`, `front` / `back` `[x, y]`,
  `top` / `bottom` `[x, z]` (as `silhouette`'s side / front / top), so
  read them off a side or plan drawing. `right` looks along -x (it cuts
  the +x side), `left` along +x, `front` along -z, `back` along +z, `top`
  down. `smooth: true` runs a curve through the points, `closed: true`
  closes the loop (a door). The output is the input with the groove cut
  (hide the input); a line that runs off the part errors with where.
  `{"type": "panel_line", "name": "body_shut", "input": "body", "view":
  "right", "points": [[1.1, 0.35], [1.15, 0.8], [0.6, 1.05]], "smooth":
  true, "width": 0.006, "depth": 0.004}`. Chain them (each reads the last
  one's output), one line per item.
- **`fill: true`** turns a closed outline into a cutter instead: a prism
  along the view from above the surface inside the outline to `depth`
  under its deepest point, so a window or lamp drawn in the view it is
  seen in lands on the surface and cuts only the near side. With `cut:
  false` (either mode) the output is the cutter itself: hide it and use it
  in a `split` (`{"cutter": "win_cut", "depth": 0.004, "lift": 0.003}` for
  flush glass) or a `boolean`.
