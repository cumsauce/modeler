# Annotations, feedback, session

**Annotations:** `add_annotation(part, text, face?, vertex?, position?, material?, node?)`; `add_annotation(resolve: id, text: "what you did")` resolves a comment you dealt with (`reopen: true` opens it again): the reply shows under it in the window, its pin greys out, and it moves under "Resolved". `scene_summary` lists the open ones in full; resolve them rather than deleting them
(omit the optionals to consume a spot the human clicked; `material` + optional `node`
comments on a procedural material or one of its nodes; humans also comment on
outliner groups, listed with their parts), `list_annotations`,
`delete_annotation(id)`.

**Feedback:** when a call surprises you (an error that didn't say what to
do, a field that didn't work as described, a workaround you needed), send
one or two sentences with `session_log(feedback: "...")`: what you tried,
what happened, what you expected. The server also logs every failed call
(error, a clip of its arguments, failures in a row) and every call over 8 s
on its own. All of it goes to `<catalog dir>/feedback.jsonl` on this machine
(git-ignored in a project's `.modeler/`); `session_log(report: true)` or
`modeler --feedback-report` folds it into failures grouped by cause (numbers
and quoted names blanked, so one problem on many parts is one line), slow
calls and notes.

**Session:** `session_log(limit?)` is the per-call record (tool, ok/error,
ms). Every mutation marks the session for autosave (ops, params, history,
annotations; op-derived parts are left out and re-derive on restore), and the
app writes `session.json` once edits pause for 0.5 s and on exit, so the write
never sits on a tool call. A server started with `--restore` (or
`MODELER_RESTORE=1`) restores it on startup; a plain `modeler` (what the
MCP config runs) starts empty and first moves the last autosave aside to
`session.prev.json` (+ `.prev.history.jsonl`, `open_docs.prev.json`, the
materials' `session.prev.json`), so its edits never overwrite it; a later
`--restore` brings the kept one back when nothing newer was written.
`mcp_driver.py` always passes `--restore`, so daemon restarts keep the work (parked, see Scenes). Each open document's history tree goes to its
own append-only journal (`session.history.<doc>.jsonl`, one node per line;
`open_docs.json` holds the inactive documents without their nodes): a save
appends only the new nodes, and rewrites a file only after undo branches
or pruning, so the autosave stays small however long the session runs and
however often agents switch documents. `save_prefab` only
names and indexes it.
