# TODO: projects and groups (as in sounder)

The home page gets crowded. Organize models the way sounder now does
(sounder commit 110565e, "Projects and groups"): a **project** is a whole
body of work (a game, the rampup project, a video series); a **group** is
any set inside it (one building's parts, the props for a level, every
weapon). Each one keeps a **style** that agents read before building, so new
models fit what is already there.

## What sounder did (copy the shape)

- **Storage:** `<library>/projects.json`, a list of
  `{name, description, style, notes: [{text, by, at}], groups: [{name,
  description, style, notes}]}`. Module: `src/projects.rs` in sounder
  (`list`, `save_all`, `check`, `context`, `valid_name`).
- **Membership on the item:** the document carries optional `project` and
  `group`. The catalog entry copies them, so listings need no file loads.
  Moving a saved item rewrites its file and the index in place, and keeps
  its approval. Membership is excluded from the render or cache hash, so
  filing never re-renders.
- **One new MCP tool, `project`:**
  - `list`: projects, groups and counts.
  - `describe`: style, all notes and members.
  - `create`: a project, or a group with `group`.
  - `update`: `description` / `style` / `rename`. A rename carries every
    filed item and each client's default along.
  - `note`: append a fact the human said.
  - `assign`: an open doc or a saved item; no `name` unfiles it.
  - `use`: a per-client default for new documents. It falls back to a
    shared default for untagged callers.
  - `delete`: refused while it has members unless `force`, which unfiles
    them.
- **Agent context:** `new_model` takes `project` / `group`. Its result
  prints the project and group style plus the latest notes, and lists the
  saved items already in the group ("match them"). `scene_summary` shows the
  same context, or "not filed under a project". `catalog_list` filters by
  `project` / `group`.
- **Docs:** a workflow step in AGENTS.md (check `project list`, file the
  new document, keep the style current with `note` / `update`), a "Projects
  and groups" section, the tool table row, and the MCP `instructions` in
  step.
- **Window home page:**
  - A chip row filters by project, or shows the unfiled items.
  - Each project, then group, is a collapsible section.
  - Each header has buttons for **Style** (an editor for description and
    style, plus a note box), **+ Group** and **+ Model** (a new document
    filed there).
  - `⋯` on a tile moves the item.
  - Unfiled items come last.
  - The filter and collapsed sections live in the page's `localStorage`.
  - The top bar shows `project / group / name`.
  - Every action goes through the agents' `project` tool, so a window edit
    and an agent edit are the same call.
- **Tests:**
  - An end-to-end driver test: create, use, new item, save, note, a second
    item sees the first item and the note, rename, filtered list, delete
    refused, unknown project named.
  - The tool appears in the "every tool rejects an unknown argument" list.
  - The window state fixture carries `projects` and each item's
    `project` / `group`.

## Modeler-specific points to decide

- Modeler's `CatalogEntry` (`src/catalog.rs`) has `name, description, tags,
  prompt, file, created_at`. Add `project` / `group` with serde defaults so
  old catalogs load.
- Prefabs placed inside other models (`load_prefab`, `prefab` items): a
  group's style is a natural home for shared materials, scale and palette.
  Consider letting `style` reference a material or style preset by name,
  so `new_model` can offer it.
- If one library serves several games, a project is the right level for
  the project-wide `style` (materials, texel density, palette) that today
  lives elsewhere. Check `describe_op("topic:style")` for overlap and
  merge rather than duplicate.
- The `scenes` topic (several models in one scene) may want a group per
  scene. Decide whether a scene is a group or something of its own.
