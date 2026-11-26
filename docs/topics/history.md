# History

**History:**

- `undo` / `redo` / `jump(id)` / `history(limit?)` / `history_prune(keep)`:
  the edit tree. Each `apply_ops` (or `cherry_pick`) call is ONE node, so
  one undo reverts the whole call. `history` shows the newest 30 nodes plus
  the cursor by default. Editing after a jump branches. With several
  agents on one server, each node records who made it (`history` shows
  `by <client>`), and `undo` refuses to revert another agent's step
  (`undo(force: true)` when that is intended).
- `diff(from, to?)`: ops added (+), removed (-) and changed (~, with the old
  parameters) between two nodes (default `to`: the live model), plus param
  changes.
- `cherry_pick(from, ops?)`: replay ops from another node onto the live model
  (all new or changed ops and params, or just the named ops/params). A
  picked op brings the param values it was built with, so the graph stays
  consistent. After a jump, this replaces re-issuing the old branch by hand.
