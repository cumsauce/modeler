# `repeat`

- **Loops (`repeat`)**: `{"type": "repeat", "var": "r", "count": 4, "items":
  [...]}` (or `"values": [0.0, 0.0025, ...]`, numbers or strings) copies its
  items once per value before style and params are read. In the copies,
  `{r}` inside any string becomes the value (`"name": "key{r}"`), `$r` in an
  expression or as a whole value is the number (`"=$h + $r * 0.0025"`), and
  `{r_i}` / `$r_i` is the index. `count` / `values` may read params
  (`"=$rows - 1"`) and repeats nest (`"count": "=4 - $row"` inside a row
  loop). Each copy is an ordinary item with its own template, so a later
  param change re-derives it. Two copies making the same part (same type and
  name) are an error naming the variable; an error inside a copy says which
  value it came from (`[from repeat r=2]`). A repeat with empty `items`,
  `count` 0 or empty `values` makes nothing (a `note:` in the result, not
  an error).
