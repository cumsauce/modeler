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
    /// The latest render per (view, size, focus, style), for `diff`.
    last_shots: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
}

/// The image budget a model sees at full detail: larger images are
/// downscaled before reading, so pixels past it cost tokens for nothing.
const MAX_LONG_EDGE: f64 = 1568.0;
const MAX_PIXELS: f64 = 1_150_000.0;

/// Shrinks a cols x rows grid of w x h cells to fit the image budget.
fn fit_budget(w: u32, h: u32, cols: u32, rows: u32) -> (u32, u32) {
    let (gw, gh) = ((cols * w) as f64, (rows * h) as f64);
    let k = (MAX_LONG_EDGE / gw)
        .min(MAX_LONG_EDGE / gh)
        .min((MAX_PIXELS / (gw * gh)).sqrt())
        .min(1.0);
    (
        ((w as f64 * k) as u32).max(64),
        ((h as f64 * k) as u32).max(64),
    )
}

impl Tools {
    pub(crate) fn new(tx: mpsc::Sender<Cmd>) -> Self {
        Self {
            tx,
            last_shots: Default::default(),
        }
    }

    async fn run(
        &self,
        f: impl FnOnce(&mut Session) -> CmdResult + Send + 'static,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_json(move |s| f(s).map(|t| (t, None)).map_err(|e| (e, None)))
            .await
    }

    /// Like [`Self::run`], but the closure may also return structured JSON,
    /// sent as the result's `structuredContent` next to the text (on success
    /// and on failure), so clients need not parse the prose.
    async fn run_json(
        &self,
        f: impl FnOnce(
            &mut Session,
        ) -> Result<
            (String, Option<serde_json::Value>),
            (modeler::ModelerError, Option<serde_json::Value>),
        > + Send
        + 'static,
    ) -> Result<CallToolResult, ErrorData> {
        let (json_tx, json_rx) = std::sync::mpsc::channel();
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = Cmd::Exec(Box::new(move |session: &mut Session| {
            let before = session.persist_fingerprint();
            let mats_before = session.materials.fingerprint();
            let reply = match f(session) {
                Ok((text, json)) => {
                    let _ = json_tx.send(json);
                    Ok(text)
                }
                Err((e, json)) => {
                    let _ = json_tx.send(json);
                    Err(e)
                }
            };
            // Autosave after every mutation so unnamed WIP survives a restart
            // (also when a batch fails midway: its earlier items stay applied);
            // read-only calls leave the fingerprint alone and skip the write.
            // The writes happen once edits pause (Session::flush_autosave).
            let model = session.persist_fingerprint() != before;
            let mats = session.materials.fingerprint() != mats_before;
            session.mark_autosave(model, false, mats);
            let _ = reply_tx.send(reply.clone());
            reply
        }));
        self.tx
            .send(cmd)
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        let reply = reply_rx.await;
        let json = json_rx.try_recv().ok().flatten();
        let with_json = |mut r: CallToolResult| {
            r.structured_content = json.clone();
            r
        };
        match reply {
            Ok(Ok(text)) => Ok(with_json(ok_result(text))),
            Ok(Err(message)) => Ok(with_json(err_result(message.to_string()))),
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
        style: ShotStyle,
    ) -> Result<(Vec<u8>, String), ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::AgentShot {
                view,
                width,
                height,
                focus,
                style,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        let png = Self::await_reply(reply_rx)
            .await
            .map_err(|e| ErrorData::internal_error(e, None))?;
        Ok((png, format!("{view} {width}x{height}")))
    }
}

impl Tools {
    /// Visible, drawable part names in model order (the `ids` colour order).
    async fn visible_parts(&self) -> Option<Vec<String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.run(move |s| {
            let names = s
                .model
                .parts
                .iter()
                .filter(|p| p.visible && !p.verts.is_empty() && !p.indices.is_empty())
                .map(|p| p.name.clone())
                .collect::<Vec<_>>();
            let _ = tx.send(names);
            Ok(String::new())
        })
        .await
        .ok()?;
        rx.try_recv().ok()
    }
}

/// Per-call session log: (tool, ok, milliseconds). Capped; the `session_log`
/// tool surfaces it so an agent can self-audit a build (failures, latency).
pub(crate) static CALL_LOG: Mutex<Vec<(String, bool, u128)>> = Mutex::new(Vec::new());

/// Records a finished call: the session log, and the friction log
/// (`modeler::feedback`) for failures and slow calls.
pub(crate) fn log_call_full(tool: &str, ok: bool, ms: u128, args: &str, error: &str) {
    let streak = {
        let log = CALL_LOG.lock().unwrap_or_else(|e| e.into_inner());
        log.iter()
            .rev()
            .take_while(|(t, o, _)| t == tool && !o)
            .count() as u32
    };
    log_call(tool, ok, ms);
    if !ok {
        modeler::feedback::record(modeler::feedback::Entry {
            kind: "error".into(),
            tool: tool.into(),
            detail: error.into(),
            args: args.into(),
            streak: streak + 1,
            ..Default::default()
        });
    } else if ms > modeler::feedback::SLOW_MS {
        modeler::feedback::record(modeler::feedback::Entry {
            kind: "slow".into(),
            tool: tool.into(),
            detail: format!("{ms} ms"),
            args: args.into(),
            ..Default::default()
        });
    }
}

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
    /// Build or edit a procedural PBR material (created if missing): an
    /// array of nodes, each {"type", "name", ...fields}, upserted by name
    /// (re-issue to tweak; {"type": "delete", "name"} removes one). Nodes read
    /// other nodes by name ("bricks", or a port "bricks.id"); constants are
    /// numbers, [r, g, b] or "#rrggbb". One `output` node names the channels
    /// (base_color, roughness, metallic, height (meters, makes the normal),
    /// normal, ao, emissive, opacity). domain "tile" (default: seamless
    /// `tile` meters square, mapped at real scale) or "object" (3D fields on
    /// the surface). Atomic: any error changes nothing. `expect` checks
    /// tileable, ranges, albedo, normals, coverage. describe_op(kind) has an
    /// example and schema for every node type.
    #[tool]
    async fn material_ops(
        &self,
        Parameters(MaterialOpsArgs {
            material,
            ops,
            params,
            domain,
            tile,
            resolution,
            expect,
        }): Parameters<MaterialOpsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            let (text, ok) = s.materials.ops(
                &material,
                ops,
                params,
                domain,
                tile,
                resolution,
                expect.as_ref(),
            )?;
            // Parts wearing this material follow the edit.
            let text = match s.refresh_pbr_users(&material) {
                Ok(0) => text,
                Ok(n) => format!(
                    "{text}\nre-applied to {n} material item(s) in model `{}`",
                    s.model.name
                ),
                Err(e) => format!("{text}\nwarning: {e}"),
            };
            if ok {
                Ok(text)
            } else {
                Err(format!("expect FAILED (the edit was applied)\n{text}").into())
            }
        })
        .await
    }

    /// Look at a material without the window: a lit sphere plus channel
    /// thumbnails (or `channels` to pick them), or `node` ("noise1",
    /// "bricks.id") to see one node's output. Rendered on the CPU from the
    /// same bake that gets exported. Default: the material last edited.
    #[tool]
    async fn material_view(
        &self,
        Parameters(MaterialViewArgs {
            material,
            node,
            channels,
            size,
            diameter,
        }): Parameters<MaterialViewArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (png_tx, png_rx) = std::sync::mpsc::channel();
        let r = self
            .run(move |s| {
                let (text, png) = s.materials.view(
                    material.as_deref(),
                    node.as_deref(),
                    channels,
                    size,
                    diameter,
                )?;
                let _ = png_tx.send(png);
                Ok(text)
            })
            .await?;
        let mut r = r;
        if let Ok(png) = png_rx.try_recv() {
            r.content.push(Content::image(B64.encode(png), "image/png"));
        }
        Ok(r)
    }

    /// The material library (shared across models, like prefabs): action
    /// "list" (saved entries), "working" (this session's materials),
    /// "describe" (a working or saved material's nodes), "save" (a working
    /// material, with description, tags, prompt), "load" (into the working
    /// set) or "delete" (moves the file to removed/).
    #[tool]
    async fn material_catalog(
        &self,
        Parameters(MaterialCatalogArgs {
            action,
            name,
            description,
            tags,
            prompt,
        }): Parameters<MaterialCatalogArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            use modeler::catalog::Kind;
            let need = |what: &str, v: &Option<String>| {
                v.clone().ok_or_else(|| format!("{action}: needs `{what}`"))
            };
            Ok(match action.as_str() {
                "list" => {
                    let e = catalog::list_kind(Kind::Materials)?;
                    if e.is_empty() {
                        "material library is empty".to_string()
                    } else {
                        format_catalog(&e).replace("catalog entries", "saved materials")
                    }
                }
                "working" => {
                    let lines: Vec<String> = s
                        .materials
                        .docs
                        .values()
                        .map(|d| {
                            format!(
                                "- `{}` v{}: {} domain, {} node(s){}",
                                d.name,
                                d.version,
                                d.domain,
                                d.nodes.len(),
                                if s.materials.active.as_deref() == Some(&d.name) {
                                    " (active)"
                                } else {
                                    ""
                                }
                            )
                        })
                        .collect();
                    if lines.is_empty() {
                        "no working materials".into()
                    } else {
                        lines.join("\n")
                    }
                }
                "describe" => {
                    let n = need("name", &name)?;
                    match s.materials.get(Some(&n)) {
                        Ok(d) => d.describe(),
                        Err(_) => {
                            let e = catalog::entry(Kind::Materials, &n)?;
                            let d: modeler::material::MatDoc =
                                catalog::load_doc(Kind::Materials, &n)?;
                            format!(
                                "saved `{}`: {} [{}]\nprompt: {}\n{}",
                                e.name,
                                e.description,
                                e.tags.join(", "),
                                e.prompt,
                                d.describe()
                            )
                        }
                    }
                }
                "save" => {
                    let tags = tags.unwrap_or_default();
                    let t: Vec<&str> = tags.iter().map(String::as_str).collect();
                    s.materials.save(
                        name.as_deref(),
                        &need("description", &description)?,
                        &t,
                        &prompt.clone().unwrap_or_default(),
                    )?
                }
                "load" => s.materials.load(&need("name", &name)?)?,
                "delete" => {
                    let n = need("name", &name)?;
                    catalog::delete_doc(Kind::Materials, &n)?;
                    format!("deleted saved material `{n}` (moved to removed/)")
                }
                other => {
                    return Err(format!(
                        "action `{other}`: use list, working, describe, save, load or delete"
                    )
                    .into());
                }
            })
        })
        .await
    }

    /// The model's design tokens, so a scene has one cohesive look instead of
    /// per-item guesses. Items then read `"@primary"` (colours, with
    /// `.darken(x)` / `.lighten(x)` / `.mix(@other, x)` / `.alpha(x)` /
    /// `.saturate(x)`), `"$bevel"` (numbers), `"style": "trim"` on a material
    /// item (a preset), and `defaults` fill fields items omit. Changing a token
    /// or loading another style re-derives every item that reads it (one undo).
    /// Actions: "show" (tokens + a swatch image), "set" (merge `tokens`),
    /// "replace", "clear", and the library: "list", "describe", "save" (name,
    /// description, tags, prompt), "load" (becomes the model's style), "delete".
    #[tool]
    async fn style(
        &self,
        Parameters(StyleArgs {
            action,
            tokens,
            name,
            description,
            tags,
            prompt,
        }): Parameters<StyleArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (png_tx, png_rx) = std::sync::mpsc::channel();
        let r = self
            .run(move |s| {
                use modeler::catalog::Kind;
                use modeler::style::Style;
                let need = |what: &str, v: &Option<String>| {
                    v.clone().ok_or_else(|| format!("{action}: needs `{what}`"))
                };
                Ok(match action.as_str() {
                    "show" => {
                        if let Ok(png) = s.model.style.swatches() {
                            let _ = png_tx.send(png);
                        }
                        s.model.style.describe()
                    }
                    "set" | "replace" => {
                        let t = tokens.ok_or("set: needs `tokens`")?;
                        let mut st = if action == "set" {
                            s.model.style.clone()
                        } else {
Style::default()
                        };
                        st.merge(&t)?;
                        let msg = s.set_style(st)?;
                        if let Ok(png) = s.model.style.swatches() {
                            let _ = png_tx.send(png);
                        }
                        format!("{msg}\n{}", s.model.style.describe())
                    }
                    "clear" => s.set_style(Style::default())?,
                    "list" => {
                        let e = catalog::list_kind(Kind::Styles)?;
                        if e.is_empty() {
                            "style library is empty".to_string()
                        } else {
                            format_catalog(&e).replace("catalog entries", "saved styles")
                        }
                    }
                    "describe" => {
                        let n = need("name", &name)?;
                        let e = catalog::entry(Kind::Styles, &n)?;
                        let st: Style = catalog::load_doc(Kind::Styles, &n)?;
                        if let Ok(png) = st.swatches() {
                            let _ = png_tx.send(png);
                        }
                        format!(
                            "saved `{}`: {} [{}]\nprompt: {}\n{}",
                            e.name,
                            e.description,
                            e.tags.join(", "),
                            e.prompt,
                            st.describe()
                        )
                    }
                    "save" => {
                        let n = name
                            .clone()
                            .or_else(|| {
                                (!s.model.style.name.is_empty()).then(|| s.model.style.name.clone())
                            })
                            .ok_or("save: needs `name`")?;
                        if s.model.style.is_empty() {
                            return Err("save: the model has no style (style set first)".into());
                        }
                        let tags = tags.unwrap_or_default();
                        let t: Vec<&str> = tags.iter().map(String::as_str).collect();
                        let mut st = s.model.style.clone();
                        st.name = n.clone();
                        let (saved, overwrote) = catalog::save_doc(
                            Kind::Styles,
                            &n,
                            &st,
                            &need("description", &description)?,
                            &t,
                            &prompt.clone().unwrap_or_default(),
                        )?;
                        s.model.style.name = saved.clone();
                        format!(
                            "saved style `{saved}`{}",
                            if overwrote { " (overwrote the previous entry)" } else { "" }
                        )
                    }
                    "load" => {
                        let n = need("name", &name)?;
                        let mut st: Style = catalog::load_doc(Kind::Styles, &n)?;
                        st.name = n.clone();
                        let msg = s.set_style(st)?;
                        if let Ok(png) = s.model.style.swatches() {
                            let _ = png_tx.send(png);
                        }
                        format!("loaded style `{n}`: {msg}\n{}", s.model.style.describe())
                    }
                    "delete" => {
                        let n = need("name", &name)?;
                        catalog::delete_doc(Kind::Styles, &n)?;
                        format!("deleted saved style `{n}` (moved to removed/)")
                    }
                    other => {
                        return Err(format!(
                            "action `{other}`: use show, set, replace, clear, list, describe, save, load or delete"
                        )
                        .into());
                    }
                })
            })
            .await?;
        let mut r = r;
        if let Ok(png) = png_rx.try_recv() {
            r.content.push(Content::image(B64.encode(png), "image/png"));
        }
        Ok(r)
    }

    /// Show the whole model: name, version, parts, triangle counts, bounds and materials.
    /// Call this first (and after any confusing result) to ground yourself in the model.
    #[tool]
    async fn scene_summary(&self) -> Result<CallToolResult, ErrorData> {
        self.run_json(|s| Ok((s.summary(), Some(s.summary_json()))))
            .await
    }

    /// Export the current model as glTF. Prefer .gltf (the default format): its
    /// baked textures go to the shared assets/textures/ folder as GPU-compressed,
    /// mipmapped KTX2 files named by content, so every model using a tile
    /// material shares one copy in memory. "glb" makes one self-contained
    /// file with the KTX2 textures inside (no sharing between files). For a game
    /// engine pass `optimize: "game"` (material dedup, contact culling,
    /// instancing of copies, weld, u16 indices, ORM textures) or an object of
    /// overrides, e.g. `{"visible_from": [{"min": [..], "max": [..]}]}` to drop
    /// faces no camera inside those boxes can see, `{"merge": "by_material",
    /// "merge_keep": ["door*"]}` for one draw call per material,
    /// `{"emissive_strength": 4}` for bloom, `{"lods": [0.5, 0.2]}` for
    /// simplified `<part>.lod1/.lod2` nodes with distance ranges in extras.lod.
    /// The result reports before -> after.
    #[tool]
    async fn export_model(
        &self,
        Parameters(ExportModelArgs {
            path,
            format,
            optimize,
        }): Parameters<ExportModelArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let format = format.unwrap_or_else(|| {
            match std::path::Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
            {
                Some("glb") => "glb".into(),
                _ => "gltf".into(),
            }
        });
        let mut path = PathBuf::from(path);
        if path.extension().is_none() {
            path.set_extension(&format);
        }
        let path = catalog::resolve_export_path(&path);
        self.run(move |s| {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let opts = modeler::export_opt::Options::parse(
                &optimize.clone().unwrap_or(serde_json::Value::Null),
            )?;
            let report = match format.as_str() {
                "glb" => gltf::export_glb_with(&s.model, &path, &opts)?,
                "gltf" => gltf::export_gltf_with(&s.model, &path, &opts)?,
                other => {
                    return Err(
                        format!("unknown format `{other}` (use \"gltf\" or \"glb\")").into(),
                    );
                }
            };
            Ok(format!(
                "exported `{}` to {}\n{report}",
                s.model.name,
                path.display()
            ))
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
                    let mut out = if overwrote {
                        format!("saved `{name}` — overwrote an existing catalog entry")
                    } else {
                        format!("saved `{name}`")
                    };
                    let (bare, flat) = modeler::commands::material_gaps(&s.model);
                    if !bare.is_empty() {
                        out += &format!(
                            "\nwarning: {} visible part(s) still on the default material ({}): give every part a material, ideally a procedural pbr one; verify {{\"materials\": true}} lists them",
                            bare.len(),
                            bare.iter().take(8).map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
                        );
                    } else if !flat.is_empty() {
                        out += &format!(
                            "\nwarning: {} visible part(s) have only a flat colour, no real material; give them a pbr material, or flat: true on the material item where a plain colour is wanted",
                            flat.len()
                        );
                    }
                    out
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
            Ok(format!("loaded prefab `{name}`\n{}", s.load_brief()))
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
        self.run(|_s| {
            let place = match catalog::project_root() {
                Some(root) => format!(
                    "library: project {} (.modeler/; relative exports go to assets/)\n",
                    root.display()
                ),
                None => "library: shared (no game project; run in one or pass --project)\n".into(),
            };
            catalog::list().map(|e| place + &format_catalog(&e))
        })
        .await
    }

    /// Describe one catalog entry in full: description, tags and the prompt that made it.
    /// `delete: true` removes the entry instead (the file moves to
    /// `prefabs/removed/`).
    #[tool]
    async fn catalog_describe(
        &self,
        Parameters(CatalogDescribeArgs { name, delete }): Parameters<CatalogDescribeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |_s| {
            if delete {
                catalog::delete_doc(catalog::Kind::Prefabs, &name)?;
                Ok(format!(
                    "removed prefab `{name}` from the catalog (its file is in prefabs/removed/)"
                ))
            } else {
                catalog::describe(&name)
            }
        })
        .await
    }

    /// Grab the current preview render as a PNG image so you can see the model.
    /// With no window (--offscreen) it renders the same camera (set_camera
    /// and auto-framing steer it) offscreen; fails in --headless mode.
    #[tool]
    async fn screenshot(&self) -> Result<CallToolResult, ErrorData> {
        self.grab_screenshot().await
    }

    /// Undo the last mutation (parts, booleans, transforms, materials, renames...).
    /// `doc: "material:<name>"` undoes that material's last edit instead.
    #[tool]
    async fn undo(
        &self,
        Parameters(DocArgs { doc }): Parameters<DocArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| match material_doc(doc.as_deref())? {
            Some(m) => Ok(s.materials.undo(m)?),
            None => s.undo(),
        })
        .await
    }

    /// Redo: re-apply the most recent undone mutation (the newest child in the
    /// history tree). If you jumped to an older state and edited, redo follows
    /// the branch you are on.
    /// `doc: "material:<name>"` acts on that material's history.
    #[tool]
    async fn redo(
        &self,
        Parameters(DocArgs { doc }): Parameters<DocArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| match material_doc(doc.as_deref())? {
            Some(m) => Ok(s.materials.redo(m)?),
            None => s.redo(),
        })
        .await
    }

    /// Show the history tree: one line per snapshot with its id, parent, label
    /// and model version; the cursor is marked `<-`. Each apply_ops / cherry_pick
    /// call is one node. Shows the newest `limit` nodes (default 30).
    #[tool]
    async fn history(
        &self,
        Parameters(HistoryArgs { limit, doc }): Parameters<HistoryArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| match material_doc(doc.as_deref())? {
            Some(m) => Ok(s.materials.history(m)?),
            None => Ok(s.history(limit)),
        })
        .await
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
        Parameters(HistoryIdArgs { id, doc }): Parameters<HistoryIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| match material_doc(doc.as_deref())? {
            Some(m) => Ok(s.materials.jump(m, id)?),
            None => s.jump(id),
        })
        .await
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

    /// What changed between two history nodes (default `to`: the live model):
    /// ops added (+), removed (-) and changed (~, with the old parameters),
    /// plus parameter changes. Use after `jump` to see what the other branch had.
    #[tool]
    async fn diff(
        &self,
        Parameters(DiffArgs { from, to }): Parameters<DiffArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.diff(from, to)).await
    }

    /// Re-apply ops from another history node onto the live model: all ops
    /// there that are new or different, or just `ops` by name. The way back
    /// after a jump: no need to re-issue the abandoned branch's ops by hand.
    #[tool]
    async fn cherry_pick(
        &self,
        Parameters(CherryPickArgs { from, ops }): Parameters<CherryPickArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.cherry_pick(from, ops)).await
    }

    /// An example payload and the JSON schema for one apply_ops item type, or
    /// (no kind) every type with an example. Cheaper than guessing field names.
    #[tool]
    async fn describe_op(
        &self,
        Parameters(DescribeOpArgs { kind }): Parameters<DescribeOpArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(match modeler::spec::describe_type(kind.as_deref()) {
            Ok(t) => ok_result(t),
            Err(e) => err_result(e),
        })
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

    /// Open documents (scenes): several models open at once, each with its own
    /// history and style; one is active and every other tool acts on it.
    /// A prefab item naming an open document instances the live doc, and
    /// switching back re-syncs instances. Actions: list, open, switch, close.
    #[tool]
    async fn scene(
        &self,
        Parameters(SceneArgs { action, name }): Parameters<SceneArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            let r = s.scene(&action, name.as_deref());
            if action != "list" && r.is_ok() {
                s.mark_autosave(true, true, false);
            }
            r
        })
        .await
    }

    /// Build or edit the model: an array of items, each tagged with a "type"
    /// (primitive, extrude, revolve, sweep, lsystem, text, hull, round, shell, group, transform,
    /// boolean, radial_array, linear_array, mirror, material, prefab,
    /// hide_part, show_part, rename, delete, tri_mesh, uvs, move, place,
    /// repeat, light, collider, marker). Ops are upserts
    /// by output name (re-issue to tweak). Items are applied in order; the
    /// batch stops at the first error, naming the item and field. Pass
    /// `expect` to verify in the same call. The result ends with a brief
    /// summary (parts added/changed/removed + warnings; summary: "full" for
    /// everything). Use describe_op(kind) for an example payload.
    #[tool]
    async fn apply_ops(
        &self,
        Parameters(ApplyOpsArgs {
            ops,
            params,
            expect,
            summary,
        }): Parameters<ApplyOpsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let full = match summary.as_deref() {
            None | Some("brief") => false,
            Some("full") => true,
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!("summary must be \"brief\" or \"full\", got `{other}`"),
                    None,
                ));
            }
        };
        self.run_json(move |s| {
            // Structured content is what many clients show instead of the
            // text: keep it brief unless asked for the full scene.
            let summary = |s: &Session, text: &str| {
                if full {
                    s.summary_json()
                } else {
                    s.brief_json(text)
                }
            };
            let text = match s.apply_values_as(params, ops, full) {
                Ok(t) => t,
                Err(e) => {
                    let json = serde_json::json!({"ok": false, "error": e.to_string()});
                    return Err((e, Some(json)));
                }
            };
            let Some(spec) = expect else {
                let json = serde_json::json!({"ok": true, "summary": summary(s, &text)});
                return Ok((text, Some(json)));
            };
            let checks = s.verify_checks(&spec);
            let ok = checks.iter().all(|c| c.1);
            let json = serde_json::json!({
                "ok": ok,
                "checks": Session::checks_json(&checks),
                "summary": summary(s, &text),
            });
            let (_, report) = s.verify(&spec);
            if ok {
                Ok((format!("{text}\nexpect: all passed\n{report}"), Some(json)))
            } else {
                Err((
                    format!("expect FAILED (the batch was applied)\n{report}\n\n{text}").into(),
                    Some(json),
                ))
            }
        })
        .await
    }

    /// Deterministic checks in one call: `parts` (exact visible part names;
    /// `"gz.*"` matches a prefix), `includes` (these exist, extras fine),
    /// `grounded` (every part reaches the ground through touching parts), `bounds` ({min, max, tol}; null skips an axis), `validate` (every part
    /// closed and outward-wound) and `rebuild` (uncached re-derivation matches
    /// the cache), `stable` (the centre of mass is over the footprint), `collisions` (true or {allow: [[a, b]], max}: no part more than max (0.2) inside another). With
    /// no arguments runs validate + rebuild. Each check
    /// reports PASS/FAIL; any FAIL makes the call an error.
    #[tool]
    async fn verify(
        &self,
        Parameters(spec): Parameters<modeler::commands::VerifySpec>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_json(move |s| {
            let checks = s.verify_checks(&spec);
            let ok = checks.iter().all(|c| c.1);
            let json = serde_json::json!({"ok": ok, "checks": Session::checks_json(&checks)});
            let (_, report) = s.verify(&spec);
            if ok {
                Ok((report, Some(json)))
            } else {
                Err((report.into(), Some(json)))
            }
        })
        .await
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
        // CPU ray cast on the model itself: works headless too.
        self.run(move |s| {
            modeler::contact::probe_text(&s.model, origin, dir, all_hits)
                .map_err(modeler::ModelerError::from)
        })
        .await
    }

    /// Render agent views: named ("front", "back", "left", "right", "top",
    /// "bottom" orthographic; "iso" perspective) or cameras you place
    /// ({yaw, pitch, dist?, target?, ortho?, fov?}; yaw 0 = from +z/front, 90 =
    /// from +x/right). Several views come back tiled into ONE labelled image
    /// (grid: false for separate images), sized to stay under ~1.15 MP.
    /// style "ids" paints each part a flat colour with a legend; dims overlays
    /// sizes and a scale bar; diff highlights what changed since the previous
    /// render of the same views. Windowed preview only.
    #[tool]
    async fn agent_views(
        &self,
        Parameters(AgentViewsArgs {
            views,
            width,
            height,
            focus,
            grid,
            style,
            dims,
            lighting,
            diff,
        }): Parameters<AgentViewsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if views.is_empty() || views.len() > 9 {
            return Err(ErrorData::invalid_params(
                "give 1..9 views, e.g. [\"front\", \"right\", \"top\", \"iso\"]",
                None,
            ));
        }
        let ids = match style.as_deref() {
            None | Some("shaded") => false,
            Some("ids") => true,
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!("style must be \"shaded\" or \"ids\", got `{other}`"),
                    None,
                ));
            }
        };
        let scene_lit = match lighting.as_deref() {
            None | Some("studio") => false,
            Some("scene") => true,
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!("lighting must be \"studio\" or \"scene\", got `{other}`"),
                    None,
                ));
            }
        };
        let shot_style = ShotStyle {
            ids,
            dims,
            scene_lit,
        };
        let mut parsed = Vec::with_capacity(views.len());
        for v in views {
            parsed.push(match v {
                ViewArg::Name(n) => {
                    parse_agent_view(&n).map_err(|e| ErrorData::invalid_params(e, None))?
                }
                ViewArg::Camera(c) => AgentView::Custom(CustomCam {
                    yaw: c.yaw,
                    pitch: c.pitch,
                    dist: c.dist,
                    target: c.target,
                    ortho: c.ortho,
                    fov: c.fov.unwrap_or(45.0),
                }),
            });
        }
        let n = parsed.len() as u32;
        let grid = grid.unwrap_or(n > 1);
        let (dw, dh) = if grid { (640, 480) } else { (1280, 720) };
        let (w0, h0) = (
            width.unwrap_or(dw).clamp(64, 4096),
            height.unwrap_or(dh).clamp(64, 4096),
        );
        let (cols, rows) = if grid {
            let c = (n as f64).sqrt().ceil() as u32;
            (c, n.div_ceil(c))
        } else {
            (1, 1)
        };
        let (width, height) = fit_budget(w0, h0, cols, rows);
        let mut notes = Vec::new();
        if (width, height) != (w0, h0) {
            notes.push(format!(
                "cells shrunk from {w0}x{h0} to {width}x{height} to stay within the ~1.15 MP image budget"
            ));
        }
        let style_key = format!(
            "{}{}",
            if ids { "ids" } else { "shaded" },
            if dims { "+dims" } else { "" }
        );
        let mut shots = Vec::with_capacity(parsed.len());
        for view in &parsed {
            let (png, _) = self
                .agent_render(*view, width, height, focus.clone(), shot_style)
                .await?;
            let key = format!("{view}|{width}x{height}|{focus:?}|{style_key}");
            let prev = self
                .last_shots
                .lock()
                .ok()
                .and_then(|mut m| m.insert(key, png.clone()));
            let label = view.to_string();
            if !diff {
                shots.push((png, label));
                continue;
            }
            let Some(prev) = prev else {
                notes.push(format!(
                    "{label}: no earlier render to diff against (rendered as is; diff the next one)"
                ));
                shots.push((png, label));
                continue;
            };
            let (before, after) = (
                decode_png(&prev).map_err(|e| ErrorData::internal_error(e, None))?,
                decode_png(&png).map_err(|e| ErrorData::internal_error(e, None))?,
            );
            let (img, frac, bb) = modeler::raster::diff(&before, &after)
                .map_err(|e| ErrorData::internal_error(e, None))?;
            let pct = frac * 100.0;
            notes.push(match bb {
                None => format!("{label}: no visible change"),
                Some([x0, y0, x1, y1]) => format!(
                    "{label}: {pct:.1}% of pixels changed (magenta = now there, cyan = was there), within px box [{x0}, {y0}]..[{x1}, {y1}]"
                ),
            });
            let out = img
                .to_png()
                .map_err(|e| ErrorData::internal_error(e, None))?;
            shots.push((out, format!("{label} diff {pct:.1}%")));
        }
        let mut head = Vec::new();
        if ids {
            let names = self.visible_parts().await.unwrap_or_default();
            head.push(format!(
                "ids colours (legend also drawn in each view): {}",
                names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| format!("{}={n}", i + 1))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        head.extend(notes);
        if grid {
            let labels: Vec<String> = shots
                .iter()
                .enumerate()
                .map(|(i, (_, l))| format!("{}: {l}", i + 1))
                .collect();
            let png = compose_grid(&shots).map_err(|e| ErrorData::internal_error(e, None))?;
            head.insert(
                0,
                format!(
                    "{} views in one image, row-major: {}",
                    shots.len(),
                    labels.join(", ")
                ),
            );
            return Ok(CallToolResult::success(vec![
                Content::text(head.join("\n")),
                Content::image(B64.encode(png), "image/png"),
            ]));
        }
        let mut content = Vec::with_capacity(shots.len() * 2 + 1);
        if !head.is_empty() {
            content.push(Content::text(head.join("\n")));
        }
        for (i, (png, label)) in shots.into_iter().enumerate() {
            content.push(Content::text(format!("view {}: {label}", i + 1)));
            content.push(Content::image(B64.encode(png), "image/png"));
        }
        Ok(CallToolResult::success(content))
    }

    /// Measure and probe the model numerically: `measure` pairs (gap,
    /// touching or overlap volume/depth between two parts; one name = its
    /// height above the ground), `section` (cut with the plane axis = at: per
    /// part area, outlines, holes, thinnest wall, plus an image of the cut)
    /// and `mass` (mass, centre of mass, and whether it tips over and at what
    /// tilt; density kg/m3 per part, default 1000). Any combination in one call.
    #[tool]
    async fn inspect(
        &self,
        Parameters(InspectArgs {
            measure,
            section,
            mass,
        }): Parameters<InspectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if measure.is_none() && section.is_none() && mass.is_none() {
            return Err(ErrorData::invalid_params(
                "give measure, section and/or mass, e.g. {\"measure\": [[\"handle\", \"cup\"]]}",
                None,
            ));
        }
        let (png_tx, png_rx) = std::sync::mpsc::channel();
        let result = self
            .run_json(move |s| {
                let model = &s.model;
                let mut text = Vec::new();
                let mut json = serde_json::Map::new();
                if let Some(pairs) = measure {
                    let mut rows = Vec::new();
                    for pair in &pairs {
                        let (t, j) =
                            modeler::inspect::measure(model, pair).map_err(|e| (e, None))?;
                        text.push(t);
                        rows.push(j);
                    }
                    json.insert("measure".into(), rows.into());
                }
                if let Some(sec) = section {
                    let (t, j, png) =
                        modeler::inspect::section(model, &sec.axis, sec.at, sec.parts.as_deref())
                            .map_err(|e| (e, None))?;
                    text.push(t.trim_end().to_string());
                    json.insert("section".into(), j);
                    let _ = png_tx.send(png);
                }
                if let Some(m) = mass {
                    let (t, j) = modeler::inspect::mass_of(
                        model,
                        m.parts.as_deref(),
                        &m.density,
                        m.default_density.unwrap_or(1000.0),
                    );
                    text.push(t.trim_end().to_string());
                    json.insert("mass".into(), j);
                }
                Ok((text.join("\n"), Some(serde_json::Value::Object(json))))
            })
            .await?;
        let mut result = result;
        if let Ok(png) = png_rx.try_recv() {
            result
                .content
                .push(Content::image(B64.encode(png), "image/png"));
        }
        Ok(result)
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
            material,
            node,
            text,
            face,
            vertex,
            position,
        }): Parameters<AddAnnotationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| match material {
            Some(material) => s.annotate(
                &modeler::AnnotationTarget::Material { material, node },
                &text,
            ),
            None => s.add_annotation(&part, &text, face, vertex, position),
        })
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
    /// `feedback: "..."` tells the maintainers what got in your way (kept on
    /// this machine with your recent calls); `report: true` shows the
    /// friction report from every session.
    #[tool]
    async fn session_log(
        &self,
        Parameters(SessionLogArgs {
            limit,
            feedback,
            report,
        }): Parameters<SessionLogArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if report {
            let text = modeler::feedback::report(&modeler::feedback::read(), None);
            return Ok(CallToolResult::success(vec![Content::text(text)]));
        }
        if let Some(note) = feedback.filter(|n| !n.trim().is_empty()) {
            let recent: Vec<String> = {
                let log = CALL_LOG.lock().unwrap_or_else(|e| e.into_inner());
                log.iter()
                    .rev()
                    .take(8)
                    .rev()
                    .map(|(t, ok, ms)| format!("{t} {} ({ms}ms)", if *ok { "ok" } else { "ERROR" }))
                    .collect()
            };
            modeler::feedback::record(modeler::feedback::Entry {
                kind: "note".into(),
                tool: recent
                    .last()
                    .map(|r| r.split(' ').next().unwrap_or("").to_string())
                    .unwrap_or_default(),
                detail: note,
                recent,
                ..Default::default()
            });
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "thanks: noted in {} with your last calls (stays on this machine)",
                modeler::feedback::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ))]));
        }
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
        let args = request
            .arguments
            .as_ref()
            .map(|a| modeler::feedback::clip(&serde_json::Value::Object(a.clone()).to_string()))
            .unwrap_or_default();
        let started = std::time::Instant::now();
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = Self::tool_router().call(tcc).await;
        let ok = result.as_ref().is_ok_and(|r| r.is_error != Some(true));
        let error = match &result {
            Err(e) => e.message.to_string(),
            Ok(r) if !ok => r
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                .collect::<Vec<_>>()
                .join("\n"),
            Ok(_) => String::new(),
        };
        log_call_full(&name, ok, started.elapsed().as_millis(), &args, &error);
        result
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        const INSTRUCTIONS: &str = "Operating guidance for this modeling server:
- 1 unit = 1 meter, always (Y up, ground at y = 0): build at real-world scale.
- Call scene_summary first (and after anything confusing): parts, bounds, ops, params, annotations and warnings. An empty warnings list means none fired.
- Build with apply_ops: one call, an array of items tagged by \"type\" (primitive, extrude, revolve, sweep, lsystem, hull, round, shell, group, transform, boolean, radial_array, linear_array, mirror, material, prefab, move, place, hide_part, show_part, rename, delete, tri_mesh, uvs, repeat, light, collider, marker). describe_op(kind) returns an example and the schema for each. Items are upserts by name + type (re-issue to tweak); item order doesn't matter (hide/show, rename and move/place run after every op, in written order, seeing every part the batch makes). A batch is atomic: any failing item rolls the whole call back. shell hollows a solid (thickness, open faces). place drops parts onto a surface, or with toward back/front/left/right mounts them against a wall.
- Don't place things by arithmetic: place {parts, on, at: [x, z]} drops parts onto a surface (and follows it when params change); move pivots take anchors like \"screen:bottom-front\" (left/right, bottom/top, back/front). Name patterns take * anywhere (\"*_raw\"). place each: true drops every matched part on its own (keys onto a sloped deck). rotation may be [x,y,z,w], {axis, degrees} or {x, y, z} degrees. A primitive with frame \"paper\" / \"paper:top-front\" is positioned in that part's own coordinates and follows it; linear_array local: true steps along the source's rotated axes.
- Loops: {\"type\": \"repeat\", \"var\": \"r\", \"count\": 4 | \"values\": [..], \"items\": [..]} copies items with {r} replaced in strings (\"key{r}\") and $r as a number in expressions; repeats nest.
- Parse errors name the item and field and nothing is applied. Sizes may be a number, a short array or {radius, height}; colours [r,g,b], [r,g,b,a], 0..255 or \"#rgb\" / \"#rrggbb\"; material input defaults to its name (in place).
- Every model gets materials: give each visible part a material item, preferably a procedural pbr one (material_ops, then {\"type\": \"material\", \"name\": part, \"pbr\": name}); a part with only a flat colour is a warning until it gets a pbr material, or \"flat\": true on its material item when the user wants a plain colour. Check with verify/expect {\"materials\": true} (no default-grey parts) or \"pbr\" (no flat colours); save_prefab warns about grey parts.
- Materials follow lineage: copies and cuts of a part inherit its in-place material wherever the material item sits.
- transform is absolute unless relative: true (offsets: translation adds, rotation composes, scale multiplies).
- radial_array defaults to include_source: true (source = item 0, count items evenly spread); axis x/z for horizontal rings.
- extrude (footprint [[x, z], ...] up y = 0..height; twist, scale_top about [0, 0]) and revolve ([[radius, y], ...] around y; degrees) make closed solids from 2D profiles; inner rings are holes. Profile entries may be arcs {\"arc\": {center, radius, from, to}}; round: r fillets profile corners.
- sweep runs a profile along a 3D path (pipes, rails, handles; closed for rings); lsystem grows branching things (trees, bushes, ferns, roots, coral) from a preset (tree/pine/bush/fern/weed/roots, vary seed) or axiom + rules (parametric too: \"A(l, w) : l > 0.1\": \"!(w)F(l)[&(40)A(l*0.6, w*0.7)]A(l*0.9, w)\"), leaf_shape/angle/curl/fold/stalk, wood in name and leaves in name_leaves; hull = convex hull of parts; round fillets/chamfers/smooths a solid's edges; group merges disjoint parts into one (array or move a set; use it instead of unioning parts that don't touch).
- Booleans run on Manifold: closed outputs, exact coplanar and curved cuts. Both inputs must be closed solids (disc/plane/washer error).
- params {\"h\": 2} plus values \"$h\" or \"=$h/2 + 0.1\": changing a param later re-derives every op that reads it, in one undo step.
- prefab items instance a catalog model; its visible parts become name.part.
- Verify in the same call with apply_ops expect {parts (\"gz.*\" globs), includes, grounded, supported, rests [[a, b]], collisions, stable, bounds {min,max,tol}, validate, rebuild, scope [patterns]}, or call verify. grounded = reaches the ground through touching parts (real geometry); supported = nothing hangs (contact below the centre of mass, or a joint); collisions = no part passes through another (grounded/supported count overlaps as joints; grounded names parts held up only by a deep overlap, \"strict\" fails them). A collisions allow entry may be {\"pair\": [a, b], \"depth\": 0.003} to excuse only shallow overlaps; avoid [x, \"*\"] catch-alls. move/place results report where the parts went, parts left behind and new overlaps. probe_ray proves openings (headless too).
- inspect answers \"how close / how thick / will it stand\": measure [[a, b]] (gap, touching, or overlap volume and depth), section {axis, at} (cut outlines, areas, thinnest wall, plus an image of the cut) and mass (centre of mass, tips over or not).
- agent_views renders named views or cameras you place ({yaw, pitch, dist?, target?, ortho?}; yaw 0 = front) and tiles several into ONE image under ~1.15 MP: [\"front\", \"right\", \"top\", \"iso\"] is one read. style \"ids\" = flat colour per part with a legend; dims = sizes + scale bar; diff = what changed since the last render of the same views. Look at an image after building.
- apply_ops results end with a brief summary (parts added/changed/removed + warnings); summary: \"full\" or scene_summary for everything. Each apply_ops call is one undo step; history shows the newest 30 nodes (limit for more).
- undo/redo/jump move a history-tree cursor; jump restores that node's ops exactly. diff(from, to?) shows what changed; cherry_pick(from, ops?) replays ops from another branch.
- When a call surprises you (an error that didn't say what to do, a field that didn't work as described, a workaround you needed), tell the maintainers in one or two sentences with session_log(feedback: \"...\"): what you tried, what happened, what you expected. Failed and slow calls are logged on their own; notes say why.
- scene_summary, apply_ops and verify also return structuredContent JSON (apply_ops: the result text plus counts; summary \"full\" for every part). material items take name patterns (\"wall_*\"); a material_ops edit re-applies itself to the parts using it. catalog_describe(name, delete: true) removes a prefab.
- collider rules (parts globs, shape box/hull/mesh/none, body, sensor) and marker items (spawns, triggers with size, data JSON) export as node extras for game code; they ride prefabs and moves.
- light items (point/spot/directional, Bevy units: lumens/lux, attach to a part) export as KHR_lights_punctual; agent_views lighting: \"scene\" renders with only the model's lights.
- scene(open/switch/list/close) keeps several documents open; a prefab item naming an open doc instances it live (re-synced on switch); prefab style: \"scene\" re-themes a piece with the scene's tokens.
- Every mutation autosaves the session; save_prefab names and indexes it (model name from set_model_name). export_model skips hidden parts.
- Render waits time out after 15s with an error.
- Style tokens (a cohesive look): set them first with style(action \"set\" or \"load\"), then build with \"@primary\" colours (.darken(x) .lighten(x) .mix(@b, x) .alpha(x)), \"$bevel\" numbers, \"style\": \"<preset>\" on material items; style defaults fill omitted fields. Prefer tokens over literal colours; follow the style's brief. Changing a token re-derives every item reading it.
- Procedural PBR materials: material_ops(material, ops) builds a node graph (coords, 2D/3D SDFs, noise, worley, math, colour; one output node with base_color, roughness, metallic, height (m), ao...), atomic like apply_ops, with expect {tileable, ranges, albedo, normals, coverage}. describe_op(\"nodes\") lists node types, \"node:<type>\" one. material_view shows a lit sphere + channels (or one node). material_catalog saves/loads the library. Put one on a part with an apply_ops material item: {\"type\": \"material\", \"name\": \"wall\", \"pbr\": \"bricks\", \"params\"?: {..}, \"scale\"?: 1.5} (tile materials map at real size and are shared: a .gltf export (the default, preferred over glb) writes each texture once into assets/textures/ as GPU-compressed mipmapped KTX2, named by content, so every model reusing the material points at one file; object materials wrap a per-part atlas no other model can share, so prefer tile for kit materials). undo/redo/jump/history take doc \"material:<name>\".
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

/// `doc` on history tools: None/"model" = the model, "material:<name>" (or
/// "material" for the active one) = a material's own history.
fn material_doc(doc: Option<&str>) -> Result<Option<Option<&str>>, String> {
    match doc {
        None | Some("model") => Ok(None),
        Some("material") => Ok(Some(None)),
        Some(d) => match d.strip_prefix("material:") {
            Some(n) => Ok(Some(Some(n))),
            None => Err(format!("doc `{d}`: use \"model\" or \"material:<name>\"")),
        },
    }
}

pub(crate) fn format_catalog(entries: &[CatalogEntry]) -> String {
    if entries.is_empty() {
        return "catalog is empty".into();
    }
    let mut out = format!("{} catalog entries\n", entries.len());
    for e in entries {
        let knobs = catalog::knobs(e);
        out += &format!(
            "- `{}`: {} [{}]{}\n",
            e.name,
            e.description,
            e.tags.join(", "),
            if knobs.is_empty() {
                String::new()
            } else {
                format!(" (knobs: {knobs})")
            }
        );
    }
    out
}
