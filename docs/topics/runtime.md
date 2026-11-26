# Bevy runtime plugin, rigged prefabs, limits

- **Runtime plugin (`crates/modeler_bevy`)**:
  - `app.add_plugins(modeler_bevy::ModelerPlugin)` runs the same wind and
    spring simulation live (`crates/modeler_anim`), after `AnimationPlayer`,
    so a spring tail layers over a playing walk. Each instance gets its own
    phase.
  - The `Wind` resource steers every rig: `enabled`, `direction`,
    `strength`, `gust`.
  - Play the baked sway clip *or* keep the live sway (`Sway::live`), not
    both.
  - It reads `extras.correctives` into `ModelerCorrectives` and sets the
    driven `MorphWeights` slots every frame from the joints' local
    rotations (after `AnimationSystems` and the sway), so a game gets the
    modeler's deformation with no setup; animated shape weights keep
    their own slots.
  - It also turns `extras.collider` into `ModelerCollider` and
    `extras.marker` into `ModelerMarker`.
  - **LODs**: `extras.lod` becomes `ModelerLod` and a `VisibilityRange`
    on the node and its mesh entities (`ModelerLods`: `crossfade` meters,
    `distance_scale`, `enabled`), so nothing is hand-written.
  - **Playing clips**: put `ModelerClips::from_gltf(&gltf, &mut graphs)`
    on the scene root (every `AnimationPlayer` below gets the graph and
    `AnimationTransitions`), then `ModelerAnimator::play(root, "run",
    Duration::from_millis(200))` from any system crossfades to the clip
    on all players, honours `extras.loop`, and swaps a sway's live
    simulation for its baked clip. `ModelerRootMotion` on the root moves its
    `Transform` by the playing clips' `extras.root_motion` (weighted while
    crossfading, wrapping on loops; the exporter removed that travel from
    the hip); `ModelerClipEvent` messages (`entity`, `clip`, `name`, `time`,
    `data`) fire once as playback crosses each `extras.events` time, also
    across loop wraps.
  - Bevy limits, measured on the dressed human (`cargo test -p modeler_bevy
    --test character -- --nocapture`): the glTF loader reads the sparse
    corrective accessors correctly (every vertex equals the file's) but
    densifies them: Bevy keeps 48 bytes per vertex per target in the main
    world and uploads the same, so the human's 14.5k vertices x 131 targets
    are 91 MB, 182 MB with the LOD meshes (each keeps every target). The
    file stays small (sparse: 6.4 MB bin, 0.14 s to load); the cost is
    runtime memory, so for many characters use `correctives: {"max": N}`.
    Bevy allows 256 targets per mesh (a human has 131).
  - A skinned character is several animation roots (the skeleton and the
    body node with the morph weights), so Bevy puts one `AnimationPlayer`
    on each; a plain loop over players misses some (`ModelerAnimator` does
    all). A bone a clip does not key keeps the pose the previous clip (or
    the live sway) left it in.
  - Variants: `ModelerVariant` builds the variant's `StandardMaterial` from
    the loaded `GltfMaterial`; loading `file.gltf#MaterialN/std` instead
    re-reads and respawns the whole glTF, because Bevy drops a material
    no primitive uses once the file has loaded.
  - `cargo run -p modeler_bevy --example viewer -- <assets dir>
    <model.gltf>` looks at an export (Space / 1-9 switch clips with a
    crossfade, V variants). `cargo test -p modeler_bevy --test character`
    builds a dressed human, exports it and checks all of the above in a
    windowless full renderer (skips without `data/human/human.bin` or a
    GPU); `cargo test --test character_export` validates the glTF itself.
- **Rigged prefabs**: a `prefab` instance `inst` of a rigged model brings
  its rig: skeletons `inst.<skeleton>` (body `<name>_rig` included) and
  clips / sways `inst.<clip>`, built from the prefab's own ops and parts
  and carried by the instance's translation, rotation and scale (rest
  frames turn with it; clip translations and radii scale by the mean
  scale), so `verify animation`, `inspect pose`, `agent_views clip`,
  `set_camera(play)` and the glTF export (one skin and animation per
  instance) take `inst.walk`. Its parts keep their prefab bindings: host
  `bind` items skip them, though a host part may bind to `inst.rig`.
  Re-saving the prefab re-derives the instance's rig.
- **Limits**:
  - A host `move` / `place` of a rigged instance's parts doesn't move its
    skeleton: place the instance with the `prefab` item's transform.
    Non-uniform instance scale poses only approximately.
  - Scattered copies of a rigged tree don't get their own skeletons. Rig
    the tree in its own document, export it, and place the copies in the
    game, where the plugin sways each one with its own phase.
  - A skin's `warps` and a `mirror` other than `"x"` are not followed by
    auto-rigs.
