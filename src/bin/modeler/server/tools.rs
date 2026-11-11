use crate::prelude::*;

pub(crate) fn ok_result(text: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text)])
}

pub(crate) fn err_result(message: String) -> CallToolResult {
    CallToolResult::error(vec![Content::text(message)])
}

/// The MCP tool surface. Every tool ships a command to the Bevy loop and awaits its
/// oneshot reply, so state changes always happen on the main thread.
pub(crate) struct Tools {
    pub(crate) tx: mpsc::Sender<Cmd>,
}

impl Tools {
    pub(crate) fn new(tx: mpsc::Sender<Cmd>) -> Self {
        Self { tx }
    }

    async fn run(
        &self,
        f: impl FnOnce(&mut Session) -> CmdResult + Send + 'static,
    ) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = Cmd::Exec(Box::new(move |session: &mut Session| {
            let before = session.persist_fingerprint();
            let reply = f(session);
            // Autosave after every mutation so unnamed WIP survives a restart;
            // read-only calls leave the fingerprint alone and skip the write.
            if reply.is_ok()
                && session.persist_fingerprint() != before
                && let Err(e) = catalog::save_session_state(&session.model)
            {
                eprintln!("session autosave failed: {e}");
            }
            let _ = reply_tx.send(reply.clone());
            reply
        }));
        self.tx
            .send(cmd)
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        match reply_rx.await {
            Ok(Ok(text)) => Ok(ok_result(text)),
            Ok(Err(message)) => Ok(err_result(message.to_string())),
            Err(_) => Err(ErrorData::internal_error(
                "preview app dropped the reply",
                None,
            )),
        }
    }

    /// Waits for a Bevy-side reply with a hard timeout: a stuck frame (or any
    /// hang in the render loop) surfaces as an error to the calling tool
    /// instead of wedging the MCP server forever.
    async fn await_reply(
        reply_rx: tokio::sync::oneshot::Receiver<Result<Vec<u8>, String>>,
    ) -> Result<Vec<u8>, String> {
        match tokio::time::timeout(std::time::Duration::from_secs(15), reply_rx).await {
            Ok(Ok(inner)) => inner,
            Ok(Err(_)) => Err("preview app dropped the request".into()),
            Err(_) => Err(
                "timed out after 15s waiting for the preview app - the render may be stuck; retry or restart the daemon".into(),
            ),
        }
    }

    async fn grab_screenshot(&self) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::Screenshot(reply_tx))
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        match Self::await_reply(reply_rx).await {
            Ok(png) => Ok(CallToolResult::success(vec![
                Content::image(B64.encode(png), "image/png"),
                Content::text("current preview render"),
            ])),
            Err(message) => Ok(err_result(message)),
        }
    }

    /// One offscreen agent shot: repositions the rig camera, waits for the
    /// capture, returns (png bytes, human label).
    async fn agent_render(
        &self,
        view: AgentView,
        width: u32,
        height: u32,
        focus: Option<Vec<String>>,
    ) -> Result<(Vec<u8>, String), ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::AgentShot {
                view,
                width,
                height,
                focus,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        let png = Self::await_reply(reply_rx)
            .await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        Ok((png, format!("{view:?} {width}x{height}")))
    }
}

/// Per-call session log: (tool, ok, milliseconds). Capped; the `session_log`
/// tool surfaces it so an agent can self-audit a build (failures, latency).
pub(crate) static CALL_LOG: Mutex<Vec<(String, bool, u128)>> = Mutex::new(Vec::new());

pub(crate) fn log_call(tool: &str, ok: bool, ms: u128) {
    if let Ok(mut log) = CALL_LOG.lock() {
        log.push((tool.to_string(), ok, ms));
        if log.len() > 500 {
            log.remove(0);
        }
    }
}

#[tool_router]
impl Tools {
    /// Show the whole model: name, version, parts, triangle counts, bounds and materials.
    /// Call this first (and after any confusing result) to ground yourself in the model.
    #[tool]
    async fn scene_summary(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.summary())).await
    }

    /// Add a named primitive part (box, sphere, cylinder, ...) with a size and a
    /// translation. The fastest way to block out a shape: add a few primitives, then
    /// refine with boolean ops.
    #[tool]
    async fn add_primitive(
        &self,
        Parameters(AddPrimitiveArgs {
            kind,
            size,
            translation,
            name,
        }): Parameters<AddPrimitiveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.add_primitive(kind, size, translation, &name))
            .await
    }

    /// Add a part from raw triangle data: vertex positions and triangle indices, with
    /// optional normals and UVs (both are computed for you when omitted: auto normals
    /// and box projection). Use for shapes primitives and booleans cannot make.
    #[tool]
    async fn add_tri_mesh(
        &self,
        Parameters(AddTriMeshArgs {
            name,
            verts,
            indices,
            normals,
            uvs,
        }): Parameters<AddTriMeshArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.add_tri_mesh(&name, verts, indices, normals, uvs))
            .await
    }

    /// Boolean-combine two parts: "union" glues a and b, "difference" subtracts b from a
    /// (holes, cutouts), "intersection" keeps only the overlap. Both input parts are
    /// consumed and replaced by the result, optionally named.
    #[tool]
    async fn boolean(
        &self,
        Parameters(BooleanArgs { op, a, b, name }): Parameters<BooleanArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.boolean(op, &a, &b, name.as_deref()))
            .await
    }

    /// Move, rotate (quaternion [x, y, z, w]) or scale one part. Fields left out stay
    /// as they are, so you can nudge just the translation.
    #[tool]
    async fn transform_part(
        &self,
        Parameters(TransformPartArgs {
            part,
            translation,
            rotation,
            scale,
        }): Parameters<TransformPartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.transform_part(&part, translation, rotation, scale))
            .await
    }

    /// Delete a part by name.
    #[tool]
    async fn delete_part(
        &self,
        Parameters(PartArgs { part }): Parameters<PartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.delete_part(&part)).await
    }

    /// Rename a part. Pick descriptive names; parts are addressed by name everywhere.
    #[tool]
    async fn rename_part(
        &self,
        Parameters(RenamePartArgs { part, to }): Parameters<RenamePartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.rename_part(&part, &to)).await
    }

    /// Set a part's material: base colour [r, g, b, a], roughness and metalness. Any
    /// field left out keeps its current value.
    #[tool]
    async fn set_material(
        &self,
        Parameters(SetMaterialArgs {
            part,
            base_color,
            roughness,
            metalness,
        }): Parameters<SetMaterialArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.set_material(&part, base_color, roughness, metalness))
            .await
    }

    /// Re-project a part's UVs with a named projection (e.g. "box", "planar").
    /// Use after reshaping a part when textures or exports need clean UVs.
    #[tool]
    async fn set_uvs(
        &self,
        Parameters(SetUvsArgs { part, projection }): Parameters<SetUvsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.set_uvs(&part, &projection)).await
    }

    /// Export the current model as glTF. Pass format "gltf" for a JSON .gltf or "glb"
    /// for a self-contained binary .glb, plus the output file path.
    #[tool]
    async fn export_model(
        &self,
        Parameters(ExportModelArgs { path, format }): Parameters<ExportModelArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = PathBuf::from(path);
        self.run(move |s| match format.as_str() {
            "glb" => gltf::export_glb(&s.model, &path)
                .map(|_| format!("exported `{}` to {}", s.model.name, path.display()))
                .map_err(modeler::ModelerError::Io),
            "gltf" => gltf::export_gltf(&s.model, &path)
                .map(|_| format!("exported `{}` to {}", s.model.name, path.display()))
                .map_err(modeler::ModelerError::Io),
            other => Err(format!("unknown format `{other}` (use \"gltf\" or \"glb\")").into()),
        })
        .await
    }

    /// Save the current model into the shared prefab catalog as JSON, recording a
    /// description, tags and the prompt that produced it. The model's own name is the
    /// catalog entry name (set it with set_model_name first if needed).
    #[tool]
    async fn save_prefab(
        &self,
        Parameters(SavePrefabArgs {
            description,
            tags,
            prompt,
        }): Parameters<SavePrefabArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
            catalog::save_prefab(&s.model, &description, &tag_refs, &prompt).map(
                |(name, overwrote)| {
                    if overwrote {
                        format!("saved `{name}` — overwrote an existing catalog entry")
                    } else {
                        format!("saved `{name}`")
                    }
                },
            )
        })
        .await
    }

    /// Load a prefab from the catalog by name, replacing the current model in the
    /// session (undoable). Use catalog_list to see what exists.
    #[tool]
    async fn load_prefab(
        &self,
        Parameters(CatalogNameArgs { name }): Parameters<CatalogNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            let model = catalog::load_prefab(&name)?;
            let mut model = model;
            model.version = s.model.version + 1;
            let snapshot = s.model.clone();
            s.model.history.record(snapshot, format!("load `{name}`"));
            s.model = model;
            Ok(format!("loaded prefab `{name}`\n{}", s.summary()))
        })
        .await
    }

    /// Name the whole model (used in summaries, exports and as the prefab name).
    #[tool]
    async fn set_model_name(
        &self,
        Parameters(NameArgs { name }): Parameters<NameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            s.model.name = name.clone();
            let snapshot = s.model.clone();
            s.model.history.record(snapshot, format!("name `{name}`"));
            Ok(format!("model renamed to `{name}`"))
        })
        .await
    }

    /// List every entry in the shared prefab catalog: names, tags and descriptions.
    #[tool]
    async fn catalog_list(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|_s| catalog::list().map(|e| format_catalog(&e)))
            .await
    }

    /// Describe one catalog entry in full: description, tags and the prompt that made it.
    #[tool]
    async fn catalog_describe(
        &self,
        Parameters(CatalogNameArgs { name }): Parameters<CatalogNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |_s| catalog::describe(&name)).await
    }

    /// Grab the current preview render as a PNG image so you can see the model.
    /// Needs the windowed preview app; fails in --headless mode.
    #[tool]
    async fn screenshot(&self) -> Result<CallToolResult, ErrorData> {
        self.grab_screenshot().await
    }

    /// Undo the last mutation (parts, booleans, transforms, materials, renames...).
    #[tool]
    async fn undo(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| s.undo()).await
    }

    /// Redo: re-apply the most recent undone mutation (the newest child in the
    /// history tree). If you jumped to an older state and edited, redo follows
    /// the branch you are on.
    #[tool]
    async fn redo(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| s.redo()).await
    }

    /// Show the history tree: one line per snapshot with its id, parent, label
    /// and model version. The cursor marks where you are.
    #[tool]
    async fn history(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.history())).await
    }

    /// Prune old history to bound memory and autosave size: keeps the newest
    /// `keep` snapshots plus the cursor, the root and every branch point, and
    /// renumbers ids. Op-driven snapshots are already small (parts are
    /// re-derived), so this mainly matters for long sessions.
    #[tool]
    async fn history_prune(
        &self,
        Parameters(HistoryPruneArgs { keep }): Parameters<HistoryPruneArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.history_prune(keep)).await
    }

    /// Jump to any snapshot in the history tree by id (from history). Useful to
    /// try an alternative from an earlier state: after jumping, the next edit
    /// branches — the old path stays reachable by jumping back.
    #[tool]
    async fn jump(
        &self,
        Parameters(HistoryIdArgs { id }): Parameters<HistoryIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.jump(id)).await
    }

    /// Add a primitive as a procedural graph step (upsert by output name —
    /// re-issuing an op with the same `name` replaces it, which is how you
    /// tweak a parameter). Prefer ops over baked parts: the graph re-derives.
    #[tool]
    async fn op_primitive(
        &self,
        Parameters(OpPrimitiveArgs {
            name,
            kind,
            size,
            translation,
            rotation,
        }): Parameters<OpPrimitiveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_primitive(&name, kind, size, translation, rotation))
            .await
    }

    /// Place/rotate/scale an earlier part into a new output as a graph step.
    #[tool]
    async fn op_transform(
        &self,
        Parameters(OpTransformArgs {
            name,
            input,
            translation,
            rotation,
            scale,
        }): Parameters<OpTransformArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_transform(&name, &input, translation, rotation, scale))
            .await
    }

    /// Boolean-combine two earlier parts as a graph step.
    #[tool]
    async fn op_boolean(
        &self,
        Parameters(OpBooleanArgs { name, op, a, b }): Parameters<OpBooleanArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_boolean(&name, op, &a, &b)).await
    }

    /// Copy an earlier part around a circle: `count` copies over `degrees`.
    /// Outputs are named name_0 .. name_{count-1}.
    #[tool]
    async fn op_radial_array(
        &self,
        Parameters(OpRadialArrayArgs {
            name,
            input,
            count,
            center,
            degrees,
            start_degrees,
            axis,
            include_source,
        }): Parameters<OpRadialArrayArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            s.op_radial_array(
                &name,
                &input,
                count,
                center,
                degrees,
                start_degrees,
                axis,
                include_source,
            )
        })
        .await
    }

    /// Re-material an earlier part as a graph step.
    #[tool]
    async fn op_material(
        &self,
        Parameters(OpMaterialArgs {
            name,
            input,
            base_color,
            roughness,
            metalness,
        }): Parameters<OpMaterialArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_material(&name, &input, base_color, roughness, metalness))
            .await
    }

    /// Copy an earlier part in a straight line as a graph step. The source
    /// counts as copy 0; outputs are name_1 .. name_{count-1}.
    #[tool]
    async fn op_linear_array(
        &self,
        Parameters(OpLinearArrayArgs {
            name,
            input,
            count,
            offset,
        }): Parameters<OpLinearArrayArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_linear_array(&name, &input, count, offset))
            .await
    }

    /// Mirror an earlier part across a plane as a graph step. The mesh is
    /// baked (verts reflected, winding flipped), so it renders correctly.
    #[tool]
    async fn op_mirror(
        &self,
        Parameters(OpMirrorArgs {
            name,
            input,
            axis,
            offset,
        }): Parameters<OpMirrorArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_mirror(&name, &input, axis, offset))
            .await
    }

    /// Remove one graph step by its output name and re-derive the model.
    #[tool]
    async fn op_delete(
        &self,
        Parameters(OpNameArgs { name }): Parameters<OpNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.op_delete(&name)).await
    }

    /// List the procedural graph: one line per op with its inputs.
    #[tool]
    async fn ops(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.describe_ops())).await
    }

    /// The full parameters of one op (size, translation, colors, counts...),
    /// looked up by output part name. Use before an upsert tweak to see what
    /// the op currently looks like.
    #[tool]
    async fn op_get(
        &self,
        Parameters(OpNameArgs { name }): Parameters<OpNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.describe_op(&name)).await
    }

    /// Replace the session with a fresh empty model. The old model stays one
    /// undo away (recorded in the history tree), so this is the clean way to
    /// start the next model without restarting the server.
    #[tool]
    async fn new_model(
        &self,
        Parameters(NameArgs { name }): Parameters<NameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.new_model(&name)).await
    }

    /// Apply a whole batch in one call: an array of objects, each tagged
    /// with a "type" field — the graph ops ("primitive", "transform",
    /// "boolean", "radial_array", "linear_array", "mirror", "material")
    /// carrying the same parameters as the individual tools, plus the baked
    /// edits "hide_part" {"part"}, "show_part" {"part"} and "rename"
    /// {"part", "to"}. Applied in order with the usual upsert/rollback
    /// semantics; stops at the first error with per-item context. The result
    /// ends with the full scene summary.
    #[tool]
    async fn apply_ops(
        &self,
        Parameters(ApplyOpsArgs { ops }): Parameters<ApplyOpsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let items: Vec<BatchItem> = ops.into_iter().map(BatchItem::from).collect();
        self.run(move |s| s.apply_batch(items)).await
    }

    /// Move the orbit camera: yaw/pitch in degrees and the distance from the
    /// model center. Marks the camera user-steered (auto-drift stops). Windowed
    /// preview only.
    #[tool]
    async fn set_camera(
        &self,
        Parameters(SetCameraArgs { yaw, pitch, dist }): Parameters<SetCameraArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::SetCamera {
                yaw,
                pitch,
                dist,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        match Self::await_reply(reply_rx).await {
            Ok(_) => {}
            Err(e) => return Err(ErrorData::internal_error(e, None)),
        }
        Ok(CallToolResult::success(vec![Content::text(format!(
            "camera set to yaw {yaw}°, pitch {pitch}°, dist {dist}"
        ))]))
    }

    /// Hide a part: it stays in the model and keeps deriving from ops, but the
    /// preview and the glTF export skip it. The clean way to get boolean
    /// intermediates out of an export without breaking the graph.
    #[tool]
    async fn hide_part(
        &self,
        Parameters(PartArgs { part }): Parameters<PartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.set_visible(&part, false)).await
    }

    /// Un-hide a previously hidden part.
    #[tool]
    async fn show_part(
        &self,
        Parameters(PartArgs { part }): Parameters<PartArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.set_visible(&part, true)).await
    }

    /// Mesh-level validation, no renderer needed: per-part edge manifoldness
    /// and winding (signed volume must be positive). Catches inverted normals
    /// and torn geometry that a screenshot only hints at.
    #[tool]
    async fn validate(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.validate())).await
    }

    /// Re-derives the op graph into a scratch copy and compares the geometry
    /// against the cached parts. Catches a stale parts cache or a
    /// non-deterministic op. Baked edits (set_material, visibility) are not
    /// compared — a refresh legitimately resets those.
    #[tool]
    async fn check_rebuild(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| s.check_rebuild()).await
    }

    /// Assert the model's part names exactly (order-insensitive). Fails with
    /// the missing and unexpected names, so you can deterministically verify
    /// "my gate is 14 parts named X..." instead of eyeballing a render.
    #[tool]
    async fn expect_parts(
        &self,
        Parameters(ExpectPartsArgs { parts }): Parameters<ExpectPartsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.expect_parts(&parts)).await
    }

    /// Assert the model's world bounds numerically — the deterministic form of
    /// "it sits on the ground", "it is centered", "the roof peaks at ~2.2".
    /// Per-axis expectations; `null` skips an axis. Over visible parts only.
    #[tool]
    async fn expect_bounds(
        &self,
        Parameters(ExpectBoundsArgs { min, max, tol }): Parameters<ExpectBoundsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.expect_bounds(min, max, tol)).await
    }

    /// Cast a ray through the scene and report the first part it hits (name,
    /// distance, point) or that it passes clean through. The numeric check for
    /// openings and occlusion: cast through a window hole to prove it is open,
    /// cast at a door fill to prove it is solid. Windowed preview only.
    #[tool]
    async fn probe_ray(
        &self,
        Parameters(ProbeRayArgs {
            origin,
            dir,
            all_hits,
        }): Parameters<ProbeRayArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::ProbeRay {
                origin,
                dir,
                all_hits,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        let text = match tokio::time::timeout(std::time::Duration::from_secs(15), reply_rx).await {
            Ok(Ok(Ok(text))) => text,
            Ok(Ok(Err(e))) => return Err(ErrorData::internal_error(e, None)),
            Ok(Err(_)) => {
                return Err(ErrorData::internal_error(
                    "preview app dropped the probe",
                    None,
                ));
            }
            Err(_) => {
                return Err(ErrorData::internal_error(
                    "timed out after 15s waiting for the preview app - the render may be stuck; retry or restart the daemon",
                    None,
                ));
            }
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Render the model from the agent's own offscreen camera — independent of
    /// the human's window view, orthographic for the six cardinal views (true
    /// proportions, auto-framed to the bounds), perspective for "iso". Returns
    /// a PNG. Windowed preview only.
    #[tool]
    async fn agent_shot(
        &self,
        Parameters(AgentShotArgs {
            view,
            width,
            height,
            focus,
        }): Parameters<AgentShotArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let view = parse_agent_view(&view).map_err(|e| ErrorData::internal_error(e, None))?;
        let png = self.agent_render(view, width, height, focus).await?;
        Ok(CallToolResult::success(vec![
            Content::image(B64.encode(png.0), "image/png"),
            Content::text(png.1),
        ]))
    }

    /// Render several agent views in one call — e.g. ["front", "right", "top"]
    /// as a three-pane check, or ["front", "iso"] for proportions plus the
    /// looks-right shot. Returns one image per view, in order. Windowed
    /// preview only.
    #[tool]
    async fn agent_views(
        &self,
        Parameters(AgentViewsArgs {
            views,
            width,
            height,
            focus,
        }): Parameters<AgentViewsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if views.is_empty() || views.len() > 6 {
            return Err(ErrorData::internal_error(
                "give 1..6 views, e.g. [\"front\", \"right\", \"top\"]",
                None,
            ));
        }
        let mut parsed = Vec::with_capacity(views.len());
        for v in &views {
            parsed.push(parse_agent_view(v).map_err(|e| ErrorData::internal_error(e, None))?);
        }
        let mut content = Vec::with_capacity(parsed.len() * 2);
        for (i, view) in parsed.iter().enumerate() {
            let (png, _) = self
                .agent_render(*view, width, height, focus.clone())
                .await?;
            content.push(Content::text(format!("view {}: {view:?}", i + 1)));
            content.push(Content::image(B64.encode(png), "image/png"));
        }
        Ok(CallToolResult::success(content))
    }

    /// Add a human-readable comment pinned to the model. Face, vertex and position are
    /// all optional; when all three are omitted and the human has clicked a spot in the
    /// live view, that click is used automatically (so prefer omitting them when a pin
    /// is pending — the summary tells you when one is). Annotations persist in prefabs
    /// and show up in every scene_summary.
    #[tool]
    async fn add_annotation(
        &self,
        Parameters(AddAnnotationArgs {
            part,
            text,
            face,
            vertex,
            position,
        }): Parameters<AddAnnotationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.add_annotation(&part, &text, face, vertex, position))
            .await
    }

    /// Read every annotation on the model: one line per comment with its id, part,
    /// text and the face/vertex it is pinned to.
    #[tool]
    async fn list_annotations(&self) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.list_annotations())).await
    }

    /// Delete one annotation by its id (from list_annotations).
    #[tool]
    async fn delete_annotation(
        &self,
        Parameters(AnnotationIdArgs { id }): Parameters<AnnotationIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.delete_annotation(id)).await
    }

    /// The recent call log: tool name, ok/error, duration in ms — so you can
    /// self-audit a build (what failed, what was slow) without leaving the
    /// server. Covers the last ~500 calls in this server process.
    #[tool]
    async fn session_log(
        &self,
        Parameters(SessionLogArgs { limit }): Parameters<SessionLogArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = limit.unwrap_or(30).min(500) as usize;
        let log = CALL_LOG.lock().unwrap_or_else(|e| e.into_inner());
        let start = log.len().saturating_sub(limit);
        let lines: Vec<String> = log[start..]
            .iter()
            .enumerate()
            .map(|(i, (tool, ok, ms))| {
                format!(
                    "#{} {} {} ({}ms)",
                    start + i + 1,
                    tool,
                    if *ok { "ok" } else { "ERROR" },
                    ms
                )
            })
            .collect();
        if lines.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "session log is empty",
            )]));
        }
        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }
}

/// The server-handler wiring: tool calls go through the generated router, and
/// `get_info` serves operating guidance in the MCP `initialize` response — the
/// standard slot for "how to drive this server", so every client lands it in
/// the model's context without anyone having to read AGENTS.md first.
#[tool_handler]
impl rmcp::ServerHandler for Tools {
    /// Dispatches through the router like the generated version, but records
    /// every call (tool, ok/error, ms) for `session_log`.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let name = request.name.to_string();
        let started = std::time::Instant::now();
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = Self::tool_router().call(tcc).await;
        let ok = result.as_ref().is_ok_and(|r| r.is_error != Some(true));
        log_call(&name, ok, started.elapsed().as_millis());
        result
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        const INSTRUCTIONS: &str = "Operating guidance for this modeling server:
- Call scene_summary first (and after anything confusing): it shows parts, bounds, ops, annotations, plus warnings for known traps. An empty warnings list means none fired.
- Build with apply_ops (a whole model in one call), not one op per call. Ops are upserts: re-issuing the same name and kind tweaks parameters and re-derives. Evaluation is dependency-ordered, so op positions don't matter.
- Wire shapes are strict: size is ALWAYS [f32;3] (sphere radius = size[0], cylinder/cone/capsule radius = size[0] height = size[1]); base_color is ALWAYS RGBA [f32;4]. Wrong shapes fail deserialization with no field name.
- op_transform values are ABSOLUTE, not additive: a part at y=0.375 transformed with translation y=0 lands at y=0. Restate the full position in every copy. op_primitive also takes an optional rotation quat [x,y,z,w].
- hide_part/show_part take an argument named part; expect_parts takes parts; save_prefab requires prompt and warns when it overwrites an existing entry.
- apply_ops batches can also include baked edits: type hide_part/show_part with part, or rename with part and to - they run in batch position.
- radial_array takes an optional axis (y default, x or z for horizontal rings like windmill blades - no manual rotation needed). Pass include_source=true to space `count` items evenly with the source as copy 0 (copies name_1..).
- probe_ray all_hits=true lists every part along the ray; agent_shot/agent_views take focus=[part names] to frame just those parts.
- Booleans run on Manifold: outputs are closed solids, coplanar and curved-on-curved cuts are exact. Both inputs must be closed solids (disc/plane/washer error). Verify pockets with probe_ray.
- session_log shows the recent per-call record (tool, ok/error, ms) for self-auditing.
- Color parts with op_material BEFORE ops that copy them (arrays/mirror/transform clone the input, so later materials leave copies default-gray - the warnings section catches this).
- Full-ring radial arrays duplicate the source unless you pass count-1 copies with start_degrees = degrees/count.
- undo/redo/jump move a history tree cursor; jump restores that snapshot's op list exactly, so re-issue every op created after the jump point. Editing after a jump branches.
- Verify deterministically before screenshots: validate (manifold/winding), expect_parts (visible parts only), expect_bounds (visible bounds), check_rebuild (stale-cache diff), probe_ray (openings/occlusion; windowed preview only). Then screenshot or agent_shot/agent_views (independent offscreen camera; front/right/top/iso need no camera setup).
- Every mutation autosaves the session (parts, ops, history) and it is restored on restart; save_prefab only names and indexes it. Prefab names come from set_model_name.
- export_model skips hidden parts; hide boolean intermediates instead of deleting them (later ops may still derive from them).
- Render waits time out after 15s with an error; a stuck frame can't wedge the server.
Full details live in AGENTS.md next to the source.";
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new("modeler", "0.1.0"))
        .with_instructions(INSTRUCTIONS)
    }
}

pub(crate) fn format_catalog(entries: &[CatalogEntry]) -> String {
    if entries.is_empty() {
        return "catalog is empty".into();
    }
    let mut out = format!("{} catalog entries\n", entries.len());
    for e in entries {
        out += &format!(
            "- `{}`: {} [{}]\n",
            e.name,
            e.description,
            e.tags.join(", ")
        );
    }
    out
}
