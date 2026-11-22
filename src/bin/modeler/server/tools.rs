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
    pub(crate) tx: crate::server::CmdSender,
    /// The latest render per (view, size, focus, style), for `diff`.
    last_shots: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    /// Per document: held from a slow call's off-thread work to its commit.
    doc_locks:
        Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

/// A tool closure's reply: text (or error) plus optional structured JSON.
type RawReply =
    Result<(String, Option<serde_json::Value>), (modeler::ModelerError, Option<serde_json::Value>)>;

/// A raw reply as the tool result (structured JSON alongside the text).
fn finish(reply: RawReply) -> CallToolResult {
    let (mut r, json) = match reply {
        Ok((text, json)) => (ok_result(text), json),
        Err((e, json)) => (err_result(e.to_string()), json),
    };
    r.structured_content = json;
    r
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

/// `detail`: "low" (the default) or "high".
fn parse_detail(detail: Option<&str>) -> Result<bool, ErrorData> {
    match detail {
        None | Some("low") => Ok(false),
        Some("high") => Ok(true),
        Some(other) => Err(ErrorData::invalid_params(
            format!("detail must be \"low\" or \"high\", got `{other}`"),
            None,
        )),
    }
}

/// A canvas of about `area` pixels with the proportions `aspect` (width over
/// height), sides in multiples of 8.
fn canvas_for(aspect: f32, area: f64) -> (u32, u32) {
    let a = aspect.clamp(0.25, 4.0) as f64;
    let side = |v: f64| (((v / 8.0).round() as u32) * 8).clamp(64, 1568);
    (side((area * a).sqrt()), side((area / a).sqrt()))
}

/// "~N image tokens" for images of these sizes.
fn cost_note(sizes: impl IntoIterator<Item = (usize, usize)>) -> String {
    let n: usize = sizes
        .into_iter()
        .map(|(w, h)| modeler::raster::image_tokens(w, h))
        .sum();
    format!("~{n} image tokens")
}

impl Tools {
    pub(crate) fn new(tx: mpsc::Sender<Cmd>) -> Self {
        Self {
            tx: crate::server::CmdSender(tx),
            last_shots: Default::default(),
            doc_locks: Default::default(),
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
        f: impl FnOnce(&mut Session) -> RawReply + Send + 'static,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(finish(self.exec_json(f).await?))
    }

    /// Runs `f` on the app thread (marking the autosave when it changed
    /// anything) and hands back its raw reply.
    async fn exec_json(
        &self,
        f: impl FnOnce(&mut Session) -> RawReply + Send + 'static,
    ) -> Result<RawReply, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = Cmd::Exec(Box::new(move |session: &mut Session| {
            let before = session.persist_fingerprint();
            let mats_before = session.materials.fingerprint();
            let reply = f(session);
            // Autosave after every mutation so unnamed WIP survives a restart
            // (also when a batch fails midway: its earlier items stay applied);
            // read-only calls leave the fingerprint alone and skip the write.
            // The writes happen once edits pause (Session::flush_autosave).
            let model = session.persist_fingerprint() != before;
            let mats = session.materials.fingerprint() != mats_before;
            session.mark_autosave(model, false, mats);
            let text = match &reply {
                Ok((t, _)) => Ok(t.clone()),
                Err((e, _)) => Err(e.clone()),
            };
            let _ = reply_tx.send(reply);
            text
        }));
        self.tx
            .send(cmd)
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        reply_rx
            .await
            .map_err(|_| ErrorData::internal_error("preview app dropped the reply", None))
    }

    /// A detached copy of the session ([`Session::snapshot`]) taken on the
    /// app thread, after `prep` ran there (reading project files needs the
    /// call's project, which begin_call sets on the app thread), plus the
    /// active document's name.
    async fn snapshot_with<T: Send + 'static>(
        &self,
        prep: impl FnOnce(&mut Session) -> T + Send + 'static,
    ) -> Result<(Box<Session>, String, T), ErrorData> {
        let (tx, rx) = oneshot::channel();
        let cmd = Cmd::Exec(Box::new(move |s: &mut Session| {
            let t = prep(s);
            let _ = tx.send((Box::new(s.snapshot()), s.model.name.clone(), t));
            Ok(String::new())
        }));
        self.tx
            .send(cmd)
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        rx.await
            .map_err(|_| ErrorData::internal_error("preview app dropped the reply", None))
    }

    /// Runs a read-only `f` on a snapshot of the session on a worker
    /// thread, so a slow check (verify, inspect) never stalls the window.
    async fn run_detached_json(
        &self,
        f: impl FnOnce(&Session) -> RawReply + Send + 'static,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(finish(self.detached_json(f).await?))
    }

    async fn detached_json(
        &self,
        f: impl FnOnce(&Session) -> RawReply + Send + 'static,
    ) -> Result<RawReply, ErrorData> {
        let (snap, _, ()) = self.snapshot_with(|_| ()).await?;
        tokio::task::spawn_blocking(move || f(&snap))
            .await
            .map_err(|e| ErrorData::internal_error(format!("worker failed: {e}"), None))
    }

    /// The per-document lock that keeps a document's slow calls in order
    /// while their heavy part runs off the app thread.
    fn doc_lock(&self, doc: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut g = self
            .doc_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(g.entry(doc.to_string()).or_default())
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

    async fn grab_screenshot(&self, width: usize) -> Result<CallToolResult, ErrorData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::Screenshot(reply_tx))
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        match Self::await_reply(reply_rx).await {
            Ok(png) => {
                let png = modeler::raster::fit_png_width(&png, width);
                let note = match modeler::raster::png_dims(&png) {
                    Some(d) => format!(
                        "current preview render, {}x{} ({})",
                        d.0,
                        d.1,
                        cost_note([d])
                    ),
                    None => "current preview render".to_string(),
                };
                Ok(CallToolResult::success(vec![
                    Content::image(B64.encode(png), "image/png"),
                    Content::text(note),
                ]))
            }
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
    /// The active model's cosmetic variant names ("default" first).
    async fn variant_names(&self) -> Vec<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self
            .run(move |s| {
                let _ = tx.send(modeler::variant::names(&s.model));
                Ok(String::new())
            })
            .await;
        let mut v = vec![modeler::variant::DEFAULT.to_string()];
        v.extend(rx.try_recv().unwrap_or_default());
        v
    }

    /// The model's contact regions, most telling first (`agent_views` joints).
    async fn joint_regions(&self, n: usize) -> Option<Vec<modeler::contact::JointRegion>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.run(move |s| {
            let _ = tx.send(modeler::contact::joint_regions(&s.model, n));
            Ok(String::new())
        })
        .await
        .ok()?;
        rx.try_recv().ok()
    }

    /// The world box of the model (or of `focus`'s parts), for sizing views.
    async fn frame_bounds(
        &self,
        focus: &Option<Vec<String>>,
    ) -> Result<Option<(Vec3, Vec3)>, ErrorData> {
        let (tx, rx) = std::sync::mpsc::channel();
        let names = focus.clone();
        self.run(move |s| {
            let b = match &names {
                Some(n) => s.model.bounds_of(n),
                None => s.model.bounds(),
            };
            let _ = tx.send(b);
            Ok(String::new())
        })
        .await?;
        Ok(rx.try_recv().ok().flatten())
    }

    /// `agent_views reference`: the silhouette in an orthographic view,
    /// compared with one or more reference images from the call's project.
    async fn reference_view(
        &self,
        refs: ReferencesArg,
        width: Option<u32>,
        height: Option<u32>,
        hi: bool,
        focus: Option<Vec<String>>,
        clip: Option<String>,
        time: Option<f32>,
    ) -> Result<CallToolResult, ErrorData> {
        let refs = match refs {
            ReferencesArg::One(r) => vec![r],
            ReferencesArg::Many(v) => v,
        };
        if refs.is_empty() || refs.len() > 4 {
            return Err(ErrorData::invalid_params(
                "reference: one {image, view} or a list of 1..4",
                None,
            ));
        }
        // World size of what the views frame (meters per pixel).
        let (tx, rx) = std::sync::mpsc::channel();
        let names = focus.clone();
        self.run(move |s| {
            let b = match &names {
                Some(n) => s.model.bounds_of(n),
                None => s.model.bounds(),
            };
            let _ = tx.send(b);
            Ok(String::new())
        })
        .await?;
        let bounds = rx.try_recv().ok().flatten();
        let dflt = if hi { 800 } else { 560 };
        let (w0, h0) = (
            width.unwrap_or(dflt).clamp(64, 2048),
            height.unwrap_or(dflt).clamp(64, 2048),
        );
        // Low detail returns each overlay only (the silhouette it is made of
        // is the grey + magenta in it); high adds the plain silhouette beside it.
        let per_ref = if hi { 2 } else { 1 };
        let (w, h) = fit_budget(w0, h0, per_ref, refs.len() as u32);
        let mut shots = Vec::new();
        let mut text = Vec::new();
        let mut json = Vec::new();
        for r in refs {
            let view_name = r.view.unwrap_or_else(|| "front".into());
            if !matches!(
                view_name.as_str(),
                "front" | "back" | "left" | "right" | "top" | "bottom"
            ) {
                return Err(ErrorData::invalid_params(
                    format!(
                        "reference.view: `{view_name}`: an orthographic view (front, back, left, right, top, bottom)"
                    ),
                    None,
                ));
            }
            let view =
                parse_agent_view(&view_name).map_err(|e| ErrorData::invalid_params(e, None))?;
            // Read the file inside the call (its project is set there).
            let (tx, rx) = std::sync::mpsc::channel();
            let file = r.image.clone();
            self.run(move |_| {
                let project = modeler::catalog::project_root();
                let got = super::ops_file::resolve_as(project.as_deref(), &file, "reference.image")
                    .and_then(|p| {
                        std::fs::read(&p).map_err(|e| format!("reference.image `{file}`: {e}"))
                    });
                let _ = tx.send(got);
                Ok(String::new())
            })
            .await?;
            let bytes = rx
                .try_recv()
                .map_err(|_| ErrorData::internal_error("reference: read did not finish", None))?
                .map_err(|e| ErrorData::invalid_params(e, None))?;
            let reference = modeler::raster::decode_image(&bytes).map_err(|e| {
                ErrorData::invalid_params(format!("reference.image `{}`: {e}", r.image), None)
            })?;
            let style = ShotStyle {
                silhouette: true,
                pose: time.map(|t| (clip.clone(), t)),
                ..ShotStyle::default()
            };
            let (png, _) = self.agent_render(view, w, h, focus.clone(), style).await?;
            let shot = decode_png(&png).map_err(|e| ErrorData::internal_error(e, None))?;
            let mask = modeler::raster::silhouette_mask(&shot);
            let d = modeler::raster::compare_silhouettes(&mask, shot.w, shot.h, &reference)
                .map_err(|e| ErrorData::invalid_params(e, None))?;
            // Meters per pixel: the model's world extent across the view
            // over its silhouette's pixel extent (the longer side).
            let m_per_px = match (bounds, modeler::raster::mask_box(&mask, shot.w)) {
                (Some((lo, hi)), Some(bb)) => {
                    let e = hi - lo;
                    let (ex, ey) = match view_name.as_str() {
                        "left" | "right" => (e.z, e.y),
                        "top" | "bottom" => (e.x, e.z),
                        _ => (e.x, e.y),
                    };
                    let (px, py) = ((bb[2] - bb[0] + 1) as f64, (bb[3] - bb[1] + 1) as f64);
                    if px >= py {
                        ex as f64 / px
                    } else {
                        ey as f64 / py
                    }
                }
                _ => 0.0,
            };
            let bands =
                modeler::raster::band_deviations(&mask, &d.fitted, shot.w, shot.h, m_per_px);
            let top = modeler::raster::top_deviations(&bands, m_per_px, 3.0, 5);
            let overlay = d
                .image
                .to_png()
                .map_err(|e| ErrorData::internal_error(e, None))?;
            let n = shots.len();
            if hi {
                shots.push((png, format!("{view_name} silhouette")));
            }
            shots.push((overlay, format!("{view_name} vs {}", r.image)));
            text.push(if hi {
                format!(
                    "{view_name}: reference `{}`: IoU {:.1}% ({}: the silhouette, {}: overlay: magenta = model only {:.1}% of pixels, cyan = reference only {:.1}%, grey = both)",
                    r.image,
                    d.iou * 100.0,
                    n + 1,
                    n + 2,
                    d.model_only * 100.0,
                    d.reference_only * 100.0
                )
            } else {
                format!(
                    "{view_name}: reference `{}`: IoU {:.1}% (cell {}: overlay: magenta = model only {:.1}% of pixels, cyan = reference only {:.1}%, grey = both)",
                    r.image,
                    d.iou * 100.0,
                    n + 1,
                    d.model_only * 100.0,
                    d.reference_only * 100.0
                )
            });
            let (ra, ma) = d.aspects;
            if (ra / ma - 1.0).abs() > 0.05 {
                text.push(format!(
                    "{view_name}: the reference's subject is {} than the model (width/height {ra:.2} vs {ma:.2}); fitted by {}",
                    if ra > ma { "wider" } else { "narrower" },
                    if ra > 1.5 { "width" } else { "height" }
                ));
            }
            if top.is_empty() {
                text.push(format!("{view_name}: no band deviates by 3% or more"));
            }
            for t in &top {
                text.push(format!("{view_name}: {}", t.describe()));
            }
            json.push(serde_json::json!({
                "view": view_name,
                "image": r.image,
                "iou": d.iou,
                "model_only": d.model_only,
                "reference_only": d.reference_only,
                "aspects": {"reference": ra, "model": ma},
                "meters_per_pixel": m_per_px,
                "deviations": top,
                "bands": bands,
            }));
        }
        let grid = compose_grid(&shots).map_err(|e| ErrorData::internal_error(e, None))?;
        if let Some(d) = modeler::raster::png_dims(&grid) {
            text.push(cost_note([d]));
        }
        let mut res = CallToolResult::success(vec![
            Content::text(text.join("\n")),
            Content::image(B64.encode(grid), "image/png"),
        ]);
        res.structured_content = Some(serde_json::json!({ "references": json }));
        Ok(res)
    }

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
    /// `tile` meters square, mapped at real scale), "object" (3D fields on
    /// the surface) or "card" (one `tile`-meter image over each card's own
    /// uvs: lsystem leaf cards) or "decal" (the same image, projected by
    /// `decal` items). opacity renders as a cutout (output
    /// alpha "mask", the default for 0/1 opacity; double-sided) or "blend".
    /// Atomic: any error changes nothing. `expect` checks
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
            ..
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
            detail,
        }): Parameters<MaterialViewArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let hi = parse_detail(detail.as_deref())?;
        let size = size.or(Some(if hi { 256 } else { 192 }));
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
            if let Some(d) = modeler::raster::png_dims(&png) {
                r.content.push(Content::text(cost_note([d])));
            }
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
    /// Actions: "show" (tokens; `image: true` adds a swatch picture), "set" (merge `tokens`),
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
            image,
            ..
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
            if image.unwrap_or(false) {
                r.content.push(Content::image(B64.encode(png), "image/png"));
            } else {
                r.content.push(Content::text(
                    "(swatch picture omitted: pass image: true to see it)",
                ));
            }
        }
        Ok(r)
    }

    /// Show the whole model: name, version, parts, triangle counts, bounds and materials.
    /// Call this first (and after any confusing result) to ground yourself in the model.
    #[tool]
    async fn scene_summary(
        &self,
        Parameters(_): Parameters<DocOnlyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
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
    /// `{"ao": {"resolution": 2048, "distance": 1.0, "samples": 64, "skip":
    /// ["ground*"]}}` (or `"ao": true` for 1024 / 1 m / 64) bakes ambient
    /// occlusion into one atlas per glTF on a second uv set (TEXCOORD_1) that
    /// every material reads as occlusionTexture texCoord 1 (instancing off).
    /// The result reports before -> after. A path ending in `.bsn` writes a
    /// level scene (jackdaw's format): prefab instances by reference (each
    /// prefab exported once to models/prefabs/), markers, the document's own
    /// geometry as one glTF, a heights file per terrain, and `components`
    /// from the project's .modeler/components.json vocabulary.
    #[tool]
    async fn export_model(
        &self,
        Parameters(ExportModelArgs {
            path,
            format,
            optimize,
            ..
        }): Parameters<ExportModelArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let format = format.unwrap_or_else(|| {
            match std::path::Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
            {
                Some("glb") => "glb".into(),
                Some("bsn") => "bsn".into(),
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
                "bsn" => {
                    let mut so = modeler::scene::SceneOptions::default();
                    if optimize.is_some() {
                        so.optimize = opts;
                    }
                    modeler::scene::export_scene(&s.model, &path, &so)?
                }
                "glb" | "gltf" => {
                    let variants = modeler::variant::names(&s.model)
                        .into_iter()
                        .map(|n| s.variant_model(&n).map(|m| (n, m)))
                        .collect::<Result<Vec<_>, String>>()?;
                    if format == "glb" {
                        gltf::export_glb_variants(&s.model, Some(&variants), &path, &opts)?
                    } else {
                        gltf::export_gltf_variants(&s.model, Some(&variants), &path, &opts)?
                    }
                }
                other => {
                    return Err(format!(
                        "unknown format `{other}` (use \"gltf\", \"glb\" or \"bsn\")"
                    )
                    .into());
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
            doc: _,
            description,
            tags,
            prompt,
            force,
        }): Parameters<SavePrefabArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
            s.save_prefab_checked(&description, &tag_refs, &prompt, force.unwrap_or(false))
                .map(
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
            if let Some(h) = catalog::prefab_file_hash(&model.name) {
                s.catalog_base.insert(catalog::sanitize(&model.name), h);
            }
            let mut model = model;
            model.version = s.model.version + 1;
            // The prefab brings its own history tree (its saved story).
            s.model = model;
            s.mark_saved();
            Ok(format!("loaded prefab `{name}`\n{}", s.load_brief()))
        })
        .await
    }

    /// Name the whole model (used in summaries, exports and as the prefab name).
    #[tool]
    async fn set_model_name(
        &self,
        Parameters(RenameModelArgs { name, .. }): Parameters<RenameModelArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| {
            let old = std::mem::replace(&mut s.model.name, name.clone());
            s.renamed_active(&old);
            let mut history = std::mem::take(&mut s.model.history);
            history.record(s.model.clone(), format!("name `{name}`"));
            s.model.history = history;
            let mut out = format!("model renamed to `{name}`");
            if let Some(other) = s.name_holder(&name) {
                out += &format!(
                    "\nwarning: another open document, `{other}`, has this name: save_prefab will refuse it (force: true overwrites that document's catalog entry); pick another name"
                );
            }
            Ok(out)
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

    /// Render your document as a PNG from the agents' camera (960 wide by
    /// default, 1280x800 with `detail: "high"`, or `width`), which set_camera
    /// yaw / pitch / dist steer; it auto-fits the model.
    /// The human's own view in the window is never moved. With
    /// `set_camera(ui: {capture: "window"})` it captures
    /// the whole window instead. Fails in --headless mode.
    #[tool]
    async fn screenshot(
        &self,
        Parameters(ScreenshotArgs { width, detail, .. }): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let hi = parse_detail(detail.as_deref())?;
        let w = width.map_or(if hi { 1280 } else { 960 }, |w| w.clamp(64, 4096)) as usize;
        self.grab_screenshot(w).await
    }

    /// Undo the last mutation (parts, booleans, transforms, materials, renames...).
    /// `doc: "material:<name>"` undoes that material's last edit instead.
    #[tool]
    async fn undo(
        &self,
        Parameters(UndoArgs { doc, force }): Parameters<UndoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| match material_doc(doc.as_deref())? {
            Some(m) => Ok(s.materials.undo(m)?),
            None => s.undo_checked(force.unwrap_or(false)),
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
        Parameters(HistoryPruneArgs { keep, .. }): Parameters<HistoryPruneArgs>,
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
    async fn ops(
        &self,
        Parameters(_): Parameters<DocOnlyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.describe_ops())).await
    }

    /// The full parameters of one op (size, translation, colors, counts...),
    /// looked up by output part name. Use before an upsert tweak to see what
    /// the op currently looks like.
    #[tool]
    async fn op_get(
        &self,
        Parameters(OpNameArgs { name, .. }): Parameters<OpNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.describe_op(&name)).await
    }

    /// What changed between two history nodes (default `to`: the live model):
    /// ops added (+), removed (-) and changed (~, with the old parameters),
    /// plus parameter changes. Use after `jump` to see what the other branch had.
    #[tool]
    async fn diff(
        &self,
        Parameters(DiffArgs { from, to, .. }): Parameters<DiffArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(move |s| s.diff(from, to)).await
    }

    /// Re-apply ops from another history node onto the live model: all ops
    /// there that are new or different, or just `ops` by name. The way back
    /// after a jump: no need to re-issue the abandoned branch's ops by hand.
    #[tool]
    async fn cherry_pick(
        &self,
        Parameters(CherryPickArgs { from, ops, .. }): Parameters<CherryPickArgs>,
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

    /// Start a fresh empty model. It never displaces work: when the current
    /// document has any ops or parts it stays open and the new one starts
    /// beside it (reach either with `doc`); only an empty document is
    /// replaced. A name that is already open is refused (scene switch to it).
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
    /// switching back re-syncs instances. Actions: list, open, switch, close,
    /// restore (a restarted server parks unsaved autosaved documents instead
    /// of reopening them: `restore` lists them, `restore` NAME opens one).
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
    /// (primitive, extrude, revolve, sweep, loft, path_array, lsystem, scatter, text, terrain, hull, blob, sdf, skin, body, silhouette, smooth, decimate, split, round, deform, shell, panel, group, transform,
    /// boolean, radial_array, linear_array, mirror, material, prefab,
    /// hide_part, show_part, rename, delete, tri_mesh, uvs, move, place,
    /// repeat, light, decal, collider, marker). Ops are upserts
    /// by output name (re-issue to tweak). Items are applied in order; the
    /// batch stops at the first error, naming the item and field. Pass
    /// `expect` to verify in the same call. The result ends with a brief
    /// summary (parts added/changed/removed + warnings; summary: "full" for
    /// everything). Use describe_op(kind) for an example payload. A big
    /// generated batch can come from `ops_file` (a JSON array of items in a
    /// file inside the project, or an absolute path when the call has no
    /// project), applied before the inline `ops`: the file
    /// is the base and inline items are tweaks that win over it.
    #[tool]
    async fn apply_ops(
        &self,
        Parameters(ApplyOpsArgs {
            doc: _,
            ops,
            ops_file,
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
        // ops_file items come first and the inline ones after them, so an
        // inline item with the same type + name (a fix) wins over the
        // file's; the expanded items are what history records. Read on the
        // app thread, where begin_call has set the call's project.
        let file = ops_file.clone();
        let prep =
            move |_: &mut Session| -> Result<(Vec<serde_json::Value>, usize, usize), String> {
                let mut batch = Vec::new();
                if let Some(file) = &file {
                    let project = modeler::catalog::project_root();
                    batch = super::ops_file::read(project.as_deref(), file)?;
                }
                let from_file = batch.len();
                let inline = ops.len();
                batch.extend(ops);
                // Silhouette outlines traced from project images become plain
                // profiles before parsing (history records the points).
                if batch
                    .iter()
                    .any(|v| v.to_string().contains("from_reference"))
                {
                    let project = modeler::catalog::project_root();
                    let read = |file: &str| -> Result<Vec<u8>, String> {
                        let p = super::ops_file::resolve_as(
                            project.as_deref(),
                            file,
                            "from_reference.image",
                        )?;
                        std::fs::read(&p).map_err(|e| format!("from_reference.image `{file}`: {e}"))
                    };
                    modeler::silhouette::resolve_references(&mut batch, &read)?;
                }
                // Mocap files: sandboxed like ops_file, parsed (errors with
                // line numbers) and hashed into the item.
                if batch.iter().any(|v| v.to_string().contains("\"mocap\"")) {
                    let project = modeler::catalog::project_root();
                    let read = |file: &str| -> Result<Vec<u8>, String> {
                        let p =
                            super::ops_file::resolve_as(project.as_deref(), file, "mocap.file")?;
                        std::fs::read(&p).map_err(|e| format!("mocap.file `{file}`: {e}"))
                    };
                    modeler::mocap::resolve_files(&mut batch, &read)?;
                }
                Ok((batch, from_file, inline))
            };
        let (mut snap, doc, prepped) = self.snapshot_with(prep).await?;
        let (batch, from_file, inline) = match prepped {
            Ok(b) => b,
            Err(msg) => {
                let json = serde_json::json!({"ok": false, "error": msg});
                return Ok(finish(Err((
                    modeler::ModelerError::Invalid(msg),
                    Some(json),
                ))));
            }
        };
        // One slow call per document at a time, in order: the off-thread
        // pass and the commit below happen under this lock.
        let lock = self.doc_lock(&doc);
        let _held = lock.lock().await;
        // The heavy part off the app thread: the same batch applied to a
        // detached copy fills the op cache (sdf meshing, booleans,
        // decimate, ...), so the real apply below re-derives from cache hits
        // in milliseconds and the window keeps drawing meanwhile. Its result
        // is thrown away: atomicity, history, authorship and rollback all
        // come from the real apply, exactly as before. A copy that went
        // stale (another edit landed in between) only costs cache misses.
        {
            let (b, p) = (batch.clone(), params.clone());
            let _ = tokio::task::spawn_blocking(move || {
                let _ = snap.apply_values_as(p, b, false);
            })
            .await;
        }
        let file = ops_file;
        let committed = self
            .exec_json(move |s| {
                if let Err(e) = s.ensure_active(&doc) {
                    let json = serde_json::json!({"ok": false, "error": e.to_string()});
                    return Err((e, Some(json)));
                }
                let dups = super::ops_file::duplicate_warnings(&batch, from_file);
                let text = match s.apply_values_as(params, batch, full) {
                    Ok(t) if dups.is_empty() => t,
                    Ok(t) => format!("{}\n{t}", dups.join("\n")),
                    Err(e) => {
                        let msg = match &file {
                            Some(file) => super::ops_file::locate_error(
                                &e.to_string(),
                                file,
                                from_file,
                                inline,
                            ),
                            None => e.to_string(),
                        };
                        let json = serde_json::json!({"ok": false, "error": msg});
                        let e = if file.is_some() {
                            modeler::ModelerError::Invalid(msg)
                        } else {
                            e
                        };
                        return Err((e, Some(json)));
                    }
                };
                // Structured content is what many clients show instead of
                // the text: keep it brief unless asked for the full scene.
                let summary = if full {
                    s.summary_json()
                } else {
                    s.brief_json(&text)
                };
                Ok((text, Some(summary)))
            })
            .await?;
        let (text, summary) = match committed {
            Ok((t, j)) => (t, j.unwrap_or_default()),
            Err(e) => return Ok(finish(Err(e))),
        };
        let Some(spec) = expect else {
            let json = serde_json::json!({"ok": true, "summary": summary});
            return Ok(finish(Ok((text, Some(json)))));
        };
        // The checks read the committed model, on a worker (still under
        // the document's lock, so no later call of this document lands
        // first).
        let checked = self
            .detached_json(move |s| {
                let (checks, report) = s.verify_run(&spec);
                let ok = checks.iter().all(|c| c.1);
                let json = serde_json::json!({
                    "ok": ok,
                    "checks": Session::checks_json(&checks),
                    "summary": summary,
                });
                if ok {
                    Ok((format!("{text}\nexpect: all passed\n{report}"), Some(json)))
                } else {
                    Err((
                        format!("expect FAILED (the batch was applied)\n{report}\n\n{text}").into(),
                        Some(json),
                    ))
                }
            })
            .await?;
        Ok(finish(checked))
    }

    /// Deterministic checks in one call: `parts` (exact visible part names;
    /// `"gz.*"` matches a prefix), `includes` (these exist, extras fine),
    /// `grounded` (every part reaches the ground through touching parts), `bounds` ({min, max, tol}; null skips an axis), `validate` (every part
    /// closed and outward-wound) and `rebuild` (uncached re-derivation matches
    /// the cache), `stable` (the centre of mass is over the footprint), `collisions` (true or {allow: [[a, b]], max}: no part more than max (0.2) inside another), relations (`coaxial`, `centered_on`, `flush`, `symmetric`, `equal_spacing`) and `density` (big bare surfaces). With
    /// no arguments runs validate + rebuild. Each check
    /// reports PASS/FAIL; any FAIL makes the call an error.
    #[tool]
    async fn verify(
        &self,
        Parameters(spec): Parameters<modeler::commands::VerifySpec>,
    ) -> Result<CallToolResult, ErrorData> {
        // Read-only: on a snapshot, off the app thread.
        self.run_detached_json(move |s| {
            let (checks, report) = s.verify_run(&spec);
            let ok = checks.iter().all(|c| c.1);
            let json = serde_json::json!({"ok": ok, "checks": Session::checks_json(&checks)});
            if ok {
                Ok((report, Some(json)))
            } else {
                Err((report.into(), Some(json)))
            }
        })
        .await
    }

    /// Aim the agents' camera that `screenshot` renders (yaw/pitch in
    /// degrees, distance from the model centre; each optional; the human's
    /// view is left alone; with --offscreen it is the one camera), and/or
    /// play a clip in the window (`play`). `ui` drives the human's window
    /// for checking the UI itself: it changes what they see, so only use
    /// it when the UI is the task.
    #[tool]
    async fn set_camera(
        &self,
        Parameters(SetCameraArgs {
            yaw,
            pitch,
            dist,
            play,
            ui,
            variant,
            ..
        }): Parameters<SetCameraArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut variant_note = None;
        if let Some(v) = variant {
            let v = (!v.is_empty()).then_some(v);
            match self.run(move |s| s.set_window_variant(v.as_deref())).await {
                Ok(r) if r.is_error == Some(true) => return Ok(r),
                Ok(r) => variant_note = Some(r),
                Err(e) => return Err(e),
            }
            if yaw.is_none() && pitch.is_none() && dist.is_none() && play.is_none() && ui.is_none()
            {
                return Ok(variant_note.unwrap_or_else(|| {
                    CallToolResult::success(vec![Content::text("variant set")])
                }));
            }
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let play = play.map(|p| crate::preview::PlayRequest {
            clip: p.clip,
            time: p.time,
            speed: p.speed,
            paused: p.paused,
        });
        self.tx
            .send(Cmd::SetCamera {
                yaw,
                pitch,
                dist,
                play,
                ui,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ErrorData::internal_error("preview app is not running", None))?;
        let msg = match Self::await_reply(reply_rx).await {
            Ok(bytes) => String::from_utf8(bytes).unwrap_or_default(),
            Err(e) => return Ok(err_result(e)),
        };
        let mut parts = Vec::new();
        if yaw.is_some() || pitch.is_some() || dist.is_some() {
            let f = |v: Option<f32>| v.map_or("unchanged".to_string(), |v| v.to_string());
            parts.push(format!(
                "camera: yaw {}, pitch {}, dist {}",
                f(yaw),
                f(pitch),
                f(dist)
            ));
        }
        if !msg.is_empty() {
            parts.push(msg);
        }
        if let Some(r) = variant_note {
            for c in r.content {
                if let Some(t) = c.as_text() {
                    parts.push(t.text.clone());
                }
            }
        }
        if parts.is_empty() {
            parts.push("nothing to do: give yaw / pitch / dist, play, ui and/or variant".into());
        }
        Ok(CallToolResult::success(vec![Content::text(
            parts.join("\n"),
        )]))
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
            ..
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
    /// (grid: false for separate images), sized to the model and to a pixel
    /// budget (`detail`: "low" by default, "high" for fine detail; a call's
    /// images stay under ~1.15 MP together, and the result says what they cost).
    /// style "silhouette" draws flat black parts on white; "density" tints
    /// surfaces by local detail (red sparse, green busy); focus "joints" (or
    /// joints: N) frames close-ups of where parts meet; reference {image,
    /// view} compares the silhouette with a PNG in the project (overlay +
    /// IoU). style "ids" paints each part a flat colour with a legend; "curvature"
    /// paints mean curvature (red convex, blue concave: lumps, pinches,
    /// creases) to judge organic surfaces; dims overlays
    /// sizes and a scale bar; diff highlights what changed since the previous
    /// render of the same views. Windowed preview only.
    #[tool]
    async fn agent_views(
        &self,
        Parameters(AgentViewsArgs {
            doc: _,
            views,
            width,
            height,
            focus,
            grid,
            style,
            dims,
            lighting,
            diff,
            clip,
            time,
            times,
            bone,
            bones: trail_bones,
            joints,
            reference,
            variant,
            variants,
            detail,
        }): Parameters<AgentViewsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let vlist: Vec<Option<String>> = match variants {
            None | Some(VariantsArg::All(false)) => vec![variant.clone()],
            Some(VariantsArg::All(true)) => {
                self.variant_names().await.into_iter().map(Some).collect()
            }
            Some(VariantsArg::Names(v)) if v.is_empty() => {
                return Err(ErrorData::invalid_params(
                    "variants: list at least one variant name",
                    None,
                ));
            }
            Some(VariantsArg::Names(v)) => v.into_iter().map(Some).collect(),
        };

        let (focus, joints) = match focus {
            Some(FocusArg::One(f)) if f == "joints" => (None, Some(joints.unwrap_or(4))),
            Some(FocusArg::One(f)) => (Some(vec![f]), joints),
            Some(FocusArg::Many(v)) => (Some(v), joints),
            None => (None, joints),
        };
        if joints.is_some_and(|n| n == 0 || n > 9) {
            return Err(ErrorData::invalid_params("joints: 1..9 close-ups", None));
        }
        if joints.is_some() && focus.is_some() {
            return Err(ErrorData::invalid_params(
                "joints frames the contact regions itself: drop focus",
                None,
            ));
        }
        if views.is_empty() || views.len() > 9 {
            return Err(ErrorData::invalid_params(
                "give 1..9 views, e.g. [\"front\", \"right\", \"top\", \"iso\"]",
                None,
            ));
        }
        let (ids, curvature, bones, weights) = match style.as_deref() {
            None | Some("shaded") | Some("silhouette") | Some("density") | Some("trails") => {
                (false, false, false, false)
            }
            Some("ids") => (true, false, false, false),
            Some("curvature") => (false, true, false, false),
            Some("bones") => (false, false, true, false),
            Some("weights") => (false, false, false, true),
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!(
                        "style must be \"shaded\", \"silhouette\", \"density\", \"ids\", \"curvature\", \"bones\", \"weights\" or \"trails\", got `{other}`"
                    ),
                    None,
                ));
            }
        };
        let silhouette = style.as_deref() == Some("silhouette");
        let density = style.as_deref() == Some("density");
        let trails = style.as_deref() == Some("trails");
        if trail_bones.is_some() && !trails {
            return Err(ErrorData::invalid_params(
                "bones belongs to style \"trails\" (the tip paths to draw); for the skeleton overlay use style \"bones\"",
                None,
            ));
        }
        let hi = parse_detail(detail.as_deref())?;
        if let Some(r) = reference {
            return self
                .reference_view(r, width, height, hi, focus, clip, time)
                .await;
        }
        if weights && bone.is_none() {
            return Err(ErrorData::invalid_params(
                "style \"weights\" needs `bone` (the bone whose weights to paint)",
                None,
            ));
        }
        if time.is_some() && times.is_some() {
            return Err(ErrorData::invalid_params(
                "give time or times, not both",
                None,
            ));
        }
        let pose_times: Vec<Option<f32>> = match (&times, time) {
            (Some(ts), _) if ts.is_empty() => {
                return Err(ErrorData::invalid_params(
                    "times: list at least one time",
                    None,
                ));
            }
            (Some(ts), _) => ts.iter().map(|t| Some(*t)).collect(),
            (None, Some(t)) => vec![Some(t)],
            (None, None) if clip.is_some() || trails => vec![Some(0.0)],
            (None, None) => vec![None],
        };
        if views.len() * pose_times.len() > 9 {
            return Err(ErrorData::invalid_params(
                format!(
                    "{} views x {} times is more than 9 images: drop views or times",
                    views.len(),
                    pose_times.len()
                ),
                None,
            ));
        }
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
            variant: None,
            pose: None,
            bones,
            weights: if weights { bone.clone() } else { None },
            ids,
            curvature,
            silhouette,
            density,
            frame: None,
            dims,
            scene_lit,
            trails: trails.then(|| trail_bones.clone()),
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
        // views x times: every view at each time; with joints, the first
        // view once per contact region instead.
        type Frame = Option<(Vec3, Vec3)>;
        let parsed: Vec<(AgentView, Option<f32>, Frame, Option<String>)> = match joints {
            Some(n) => {
                if pose_times.len() > 1 {
                    return Err(ErrorData::invalid_params(
                        "joints renders one view per contact region: give time, not times",
                        None,
                    ));
                }
                let regions = self.joint_regions(n as usize).await.unwrap_or_default();
                if regions.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "joints: no two visible parts touch or overlap (nothing to frame)",
                    )]));
                }
                let view = parsed[0];
                regions
                    .into_iter()
                    .map(|r| {
                        (
                            view,
                            pose_times[0],
                            Some((r.center - r.half, r.center + r.half)),
                            Some(r.label()),
                        )
                    })
                    .collect()
            }
            None => pose_times
                .iter()
                .flat_map(|t| parsed.iter().map(move |v| (*v, *t, None, None)))
                .collect(),
        };
        if parsed.len() * vlist.len() > 9 {
            return Err(ErrorData::invalid_params(
                format!(
                    "{} images x {} variants is more than 9: drop views or variants",
                    parsed.len(),
                    vlist.len()
                ),
                None,
            ));
        }
        let parsed: Vec<_> = vlist
            .iter()
            .flat_map(|var| {
                parsed
                    .iter()
                    .map(move |(v, t, f, j)| (*v, *t, *f, j.clone(), var.clone()))
            })
            .collect();
        let n = parsed.len() as u32;
        let grid = grid.unwrap_or(n > 1);
        let (cols, rows) = if grid {
            let c = (n as f64).sqrt().ceil() as u32;
            (c, n.div_ceil(c))
        } else {
            (1, 1)
        };
        // Canvases: sized to the model's own proportions when the caller
        // gave no size (a tall figure doesn't need a wide frame), with a
        // pixel budget set by `detail`.
        let auto = width.is_none() && height.is_none();
        let bounds = if auto {
            self.frame_bounds(&focus).await?
        } else {
            None
        };
        let (area_cell, area_one) = if hi {
            (307_200.0, 921_600.0)
        } else {
            (150_528.0, 451_584.0)
        };
        let mut notes = Vec::new();
        let mut sizes: Vec<(u32, u32)> = if auto && grid {
            // One cell size for the tiles: the geometric mean of the views'
            // natural proportions.
            let mean = (parsed
                .iter()
                .map(|(v, _, f, ..)| natural_aspect(*v, f.or(bounds)).clamp(0.4, 2.5).ln())
                .sum::<f32>()
                / n as f32)
                .exp();
            let (w0, h0) = canvas_for(mean, area_cell);
            let (w, h) = fit_budget(w0, h0, cols, rows);
            notes.push(format!("cells framed to the model: {w}x{h} each"));
            vec![(w, h); n as usize]
        } else if auto {
            parsed
                .iter()
                .map(|(v, _, f, ..)| canvas_for(natural_aspect(*v, f.or(bounds)), area_one))
                .collect()
        } else {
            let (dw, dh) = match (grid, hi) {
                (true, true) => (640, 480),
                (true, false) => (448, 336),
                (false, true) => (1280, 720),
                (false, false) => (896, 504),
            };
            let (w0, h0) = (
                width.unwrap_or(dw).clamp(64, 4096),
                height.unwrap_or(dh).clamp(64, 4096),
            );
            if grid {
                let (w, h) = fit_budget(w0, h0, cols, rows);
                if (w, h) != (w0, h0) {
                    notes.push(format!(
                        "cells shrunk from {w0}x{h0} to {w}x{h} to stay within the ~1.15 MP image budget"
                    ));
                }
                vec![(w, h); n as usize]
            } else {
                vec![(w0, h0); n as usize]
            }
        };
        if !grid {
            // Separate images share one budget, and each keeps the long-edge cap.
            let total: f64 = sizes.iter().map(|(w, h)| *w as f64 * *h as f64).sum();
            let k = (MAX_PIXELS / total).sqrt().min(1.0);
            let before = sizes.clone();
            for (w, h) in &mut sizes {
                let ke = (MAX_LONG_EDGE / (*w).max(*h) as f64).min(1.0);
                let ks = k.min(ke);
                *w = ((*w as f64 * ks) as u32).max(64);
                *h = ((*h as f64 * ks) as u32).max(64);
            }
            if sizes != before && !auto {
                notes.push(format!(
                    "images shrunk to {}x{} to stay within the ~1.15 MP total for the call",
                    sizes[0].0, sizes[0].1
                ));
            }
        }
        let style_key = format!(
            "{}{}",
            style.as_deref().unwrap_or("shaded"),
            if dims { "+dims" } else { "" }
        );
        let mut shots = Vec::with_capacity(parsed.len());
        for (idx, (view, t, frame, joint, var)) in parsed.iter().enumerate() {
            let (width, height) = sizes[idx];
            let mut st = shot_style.clone();
            st.pose = t.map(|t| (clip.clone(), t));
            st.frame = *frame;
            st.variant = var.clone().filter(|v| v != modeler::variant::DEFAULT);
            let (png, _) = self
                .agent_render(*view, width, height, focus.clone(), st)
                .await?;
            let tl = t.map(|t| format!(" t={t}")).unwrap_or_default();
            let key = format!(
                "{view}|{width}x{height}|{focus:?}|{style_key}|{clip:?}{tl}|{bone:?}{trail_bones:?}|{frame:?}|{var:?}"
            );
            let tl = match var {
                Some(v) => format!("{tl} [{v}]"),
                None => tl,
            };
            let prev = self
                .last_shots
                .lock()
                .ok()
                .and_then(|mut m| m.insert(key, png.clone()));
            let label = match joint {
                Some(j) => format!("{view}{tl} joint {j}"),
                None => format!("{view}{tl}"),
            };
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
            let Some([x0, y0, x1, y1]) = bb else {
                // Nothing moved: the note says so, and no picture is spent on it.
                notes.push(format!("{label}: no visible change (no image)"));
                continue;
            };
            notes.push(format!(
                "{label}: {pct:.1}% of pixels changed (magenta = now there, cyan = was there), shown cropped to px box [{x0}, {y0}]..[{x1}, {y1}] of the {width}x{height} render"
            ));
            let m = 12;
            let img = img.crop([x0.saturating_sub(m), y0.saturating_sub(m), x1 + m, y1 + m]);
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
        if shots.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                head.join("\n"),
            )]));
        }
        // Diffs are cropped to what changed, so they are returned one by one.
        if grid && !diff {
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
            if let Some(d) = modeler::raster::png_dims(&png) {
                head.push(cost_note([d]));
            }
            return Ok(CallToolResult::success(vec![
                Content::text(head.join("\n")),
                Content::image(B64.encode(png), "image/png"),
            ]));
        }
        head.push(cost_note(
            shots
                .iter()
                .filter_map(|(png, _)| modeler::raster::png_dims(png)),
        ));
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
    /// part area, outlines, holes, thinnest wall as numbers; `image: true`
    /// adds a picture of the cut)
    /// and `mass` (mass, centre of mass, and whether it tips over and at what
    /// tilt; density kg/m3 per part, default 1000). Any combination in one call.
    #[tool]
    async fn inspect(
        &self,
        Parameters(InspectArgs {
            doc: _,
            image,
            detail,
            measure,
            section,
            mass,
            thickness,
            pose,
            proportions,
            curves,
        }): Parameters<InspectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if measure.is_none()
            && section.is_none()
            && mass.is_none()
            && thickness.is_none()
            && pose.is_none()
            && proportions.is_none()
            && curves.is_none()
        {
            return Err(ErrorData::invalid_params(
                "give measure, section, mass, thickness, proportions, curves and/or pose, e.g. {\"measure\": [[\"handle\", \"cup\"]]}",
                None,
            ));
        }
        let (png_tx, png_rx) = std::sync::mpsc::channel();
        let result = self
            .run_detached_json(move |s| {
                let mut text = Vec::new();
                let mut json = serde_json::Map::new();
                let posed;
                let model = match &pose {
                    Some(p) => {
                        let (t, j) = modeler::anim::check::pose_report(
                            &s.model,
                            p.clip.as_deref(),
                            p.time,
                            p.bones.as_deref(),
                        )
                        .map_err(|e| (e, None))?;
                        text.push(t);
                        json.insert("pose".into(), j);
                        posed = modeler::anim::check::posed(&s.model, p.clip.as_deref(), p.time)
                            .map_err(|e| (e, None))?;
                        &posed
                    }
                    None => &s.model,
                };
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
                if let Some(cu) = curves {
                    let (t, j, png) = modeler::inspect::motion::report(
                        model,
                        cu.clip.as_deref(),
                        cu.bones.as_deref(),
                        cu.channels.as_deref(),
                        cu.samples,
                    )
                    .map_err(|e| (e, None))?;
                    text.push(t);
                    json.insert("curves".into(), j);
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
                if let Some(t) = thickness {
                    let (t, j) = modeler::organic::thickness_report(model, t.parts.as_deref());
                    text.push(t);
                    json.insert("thickness".into(), j);
                }
                if let Some(pr) = proportions {
                    let (t, j) = modeler::inspect::proportions(model, pr.parts.as_deref())
                        .map_err(|e| (e.into(), None))?;
                    text.push(t);
                    json.insert("proportions".into(), j);
                }
                Ok((text.join("\n"), Some(serde_json::Value::Object(json))))
            })
            .await?;
        let mut result = result;
        let hi = parse_detail(detail.as_deref())?;
        let mut left_out = 0;
        while let Ok(png) = png_rx.try_recv() {
            if image.unwrap_or(false) {
                let png = if hi {
                    png
                } else {
                    modeler::raster::shrink_png(&png, 0.8)
                };
                if let Some(d) = modeler::raster::png_dims(&png) {
                    result.content.push(Content::text(cost_note([d])));
                }
                result
                    .content
                    .push(Content::image(B64.encode(png), "image/png"));
            } else {
                left_out += 1;
            }
        }
        if left_out > 0 {
            result.content.push(Content::text(format!(
                "({left_out} picture(s) of the cut / plot left out: the numbers above answer it; pass image: true to see it)"
            )));
        }
        Ok(result)
    }

    /// Add a human-readable comment pinned to the model. Face, vertex and position are
    /// all optional; when all three are omitted and the human has clicked a spot in the
    /// live view, that click is used automatically (so prefer omitting them when a pin
    /// is pending — the summary tells you when one is). Annotations persist in prefabs
    /// and show up in every scene_summary.
    ///
    /// When you have dealt with a human's comment, resolve it:
    /// `{resolve: id, text: "what you did"}` (the reply shows under the
    /// comment in the window and its pin greys out). Don't delete comments
    /// you worked on; resolve them.
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
            resolve,
            reopen,
            ..
        }): Parameters<AddAnnotationArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(id) = resolve {
            return self
                .run(move |s| s.resolve_annotation(id, &text, reopen))
                .await;
        }
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
    async fn list_annotations(
        &self,
        Parameters(_): Parameters<DocOnlyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|s| Ok(s.list_annotations())).await
    }

    /// Delete one annotation by its id (from list_annotations).
    #[tool]
    async fn delete_annotation(
        &self,
        Parameters(AnnotationIdArgs { id, .. }): Parameters<AnnotationIdArgs>,
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
        // The driver daemon tags each call with its agent (`_meta`
        // "modeler/client", and "modeler/doc" for --doc): that agent's own
        // document comes back in front, and its edits are credited to it.
        let meta = |k: &str| -> Option<String> {
            request
                .meta
                .as_ref()
                .and_then(|m| m.get(k).cloned())
                .or_else(|| context.meta.get(k).cloned())
                .and_then(|v| v.as_str().map(String::from))
        };
        let (client, mut pin) = (meta("modeler/client"), meta("modeler/doc"));
        // A `doc` argument pins this one call to an open document, the way
        // `_meta` "modeler/doc" does, but refuses a document that isn't
        // open: subagents of one session share a client id (one proxy), so
        // only the argument tells their documents apart.
        let arg_doc = DOC_TOOLS
            .contains(&name.as_str())
            .then(|| {
                request
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("doc"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .flatten()
            .filter(|d| d != "model" && d != "material" && !d.starts_with("material:"));
        let project =
            meta("modeler/project").map(|p| (!p.is_empty()).then(|| std::path::PathBuf::from(p)));
        let mut doc_note = None;
        if let Some(d) = &arg_doc {
            let (want, (tx, rx)) = (d.clone(), oneshot::channel());
            let untagged = client.is_none();
            let proj = project.clone();
            let cmd = Cmd::Exec(Box::new(move |s: &mut Session| {
                // Catalog / parked lookups need the call's project.
                if proj.is_some() {
                    modeler::catalog::set_call_project(proj);
                }
                let r = match s.open_for_doc(&want) {
                    Err(e) => Err(e),
                    Ok(note) => {
                        if untagged && s.model.name != want {
                            s.scene("switch", Some(&want))
                                .map(|_| note)
                                .map_err(|e| e.to_string())
                        } else {
                            Ok(note)
                        }
                    }
                };
                let _ = tx.send(r);
                Ok(String::new())
            }));
            let checked = match self.tx.send(cmd).await {
                Ok(()) => rx.await.unwrap_or(Ok(None)),
                Err(_) => Ok(None),
            };
            if let Ok(n) = &checked {
                doc_note = n.clone();
            }
            if let Err(e) = checked {
                log_call_full(&name, false, started.elapsed().as_millis(), &args, &e);
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::Content::text(e),
                ]));
            }
            pin = arg_doc.clone();
        }
        // The caller's game project ("" = the shared library): one server
        // serves every project, switching per call.
        // Bookkeeping is queued, not awaited one by one: the app loop drains
        // the whole queue in one frame, so begin_call, the tool's own work
        // and end_call run together (each await used to cost a frame).
        let note_rx = match &client {
            Some(c) => {
                let (c, pin) = (c.clone(), pin.clone());
                let (tx, rx) = oneshot::channel();
                let cmd = Cmd::Exec(Box::new(move |s: &mut Session| {
                    let _ = tx.send(s.begin_call(Some(&c), pin.as_deref(), project));
                    Ok(String::new())
                }));
                if self.tx.send(cmd).await.is_err() {
                    None
                } else {
                    Some(rx)
                }
            }
            None => None,
        };
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let mut result = Self::tool_router().call(tcc).await;
        let note = match note_rx {
            Some(rx) => rx.await.ok().flatten(),
            None => None,
        };
        let ending = client.clone();
        let _ = self
            .tx
            .send(Cmd::Exec(Box::new(move |s: &mut Session| {
                if ending.is_some() {
                    s.end_call(ending.as_deref());
                }
                let closed = s.close_idle(modeler::commands::IDLE_CLOSE);
                if !closed.is_empty() {
                    eprintln!(
                        "closed {} idle saved document(s): {}",
                        closed.len(),
                        closed.join(", ")
                    );
                }
                Ok(String::new())
            })))
            .await;
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
        // The agent note goes in after the error is read: logged first, it
        // became the failure's "cause" and hid the real error.
        let note = match (doc_note, note) {
            (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
            (a, b) => a.or(b),
        };
        if let (Some(note), Ok(r)) = (note, result.as_mut()) {
            r.content.insert(0, rmcp::model::Content::text(note));
        }
        log_call_full(&name, ok, started.elapsed().as_millis(), &args, &error);
        result
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        const INSTRUCTIONS: &str = "Operating guidance for this modeling server:
- 1 unit = 1 meter, always (Y up, ground at y = 0): build at real-world scale.
- Call scene_summary first (and after anything confusing): parts, bounds, ops, params, annotations and warnings. An empty warnings list means none fired.
- The human's comments (annotations) are requests: scene_summary lists the open ones. When you have dealt with one, resolve it with add_annotation {resolve: id, text: what you did}; don't delete it.
- Build with apply_ops: one call, an array of items tagged by \"type\" (primitive, extrude, revolve, sweep, loft, path_array, lsystem, scatter, text, terrain, hull, blob, sdf, skin, body, silhouette, smooth, decimate, split, round, deform, shell, panel, group, transform, boolean, radial_array, linear_array, mirror, material, prefab, move, place, hide_part, show_part, rename, delete, tri_mesh, uvs, repeat, light, decal, collider, marker, variant, skeleton, bind, clip, mocap, sway, garment, carve). describe_op(kind) returns an example and the schema for each. Items are upserts by name + type (re-issue to tweak); item order doesn't matter (hide/show, rename and move/place run after every op, in written order, seeing every part the batch makes). A batch is atomic: any failing item rolls the whole call back. shell hollows a solid (thickness, open faces). garment cuts clothing out of a body's surface ({\"type\": \"garment\", \"name\", \"on\": body, \"preset\": tshirt | trousers | shoes | skirt | dress | ... | hair_short, or region {zones, arms, legs, above, below, planes}}), hides the skin it covers, and outfits are variants with show / hide part lists. Avoid knife edges: box/cylinder/wedge primitives take round: r (meters, rounded edges, still a closed solid); round fillet takes edges: top/bottom/vertical/horizontal; panel recesses a rows x cols grid of panels into a face (face, inset, depth, gap). place drops parts onto a surface, or with toward back/front/left/right mounts them against a wall. path_array with on: \"ground\" drops a row onto uneven ground (posts upright; follow: \"slope\" pitches copies along it; between: true stretches copies post to post: fence rails). decal projects a material_ops image (domain \"decal\", opacity cuts the shape) onto surfaces: {\"type\": \"decal\", \"name\", \"pbr\", \"size\": [w, h], \"on\": part, \"toward\", \"at\"} (graffiti, posters, stains, road lines).
- Don't place things by arithmetic: place {parts, on, at: [x, z] or an anchor \"desk:top-front\"} drops parts onto a surface (and follows it when params change); move pivots take anchors like \"screen:bottom-front\" (left/right, bottom/top, back/front). Name patterns take * anywhere (\"*_raw\"). place each: true drops every matched part on its own (keys onto a sloped deck). rotation may be [x,y,z,w], {axis, degrees} or {x, y, z} degrees. A primitive with frame \"paper\" / \"paper:top-front\" is positioned in that part's own coordinates and follows it; linear_array local: true steps along the source's rotated axes.
- Signs and labels: uvs projection \"face\" (alias \"label\") stretches one upright, unmirrored image over each side of a part (planar now maps along the thinnest axis); a card material with an opacity cutout on such a closed plate draws single-sided (open sheets such as leaf cards stay double-sided). ops_file items apply first and inline ops after, so an inline item with the same type + name wins (the result warns about every op given twice). An empty repeat is a noted no-op.
- Loops: {\"type\": \"repeat\", \"var\": \"r\", \"count\": 4 | \"values\": [..], \"items\": [..]} copies items with {r} replaced in strings (\"key{r}\") and $r as a number in expressions; repeats nest.
- Parse errors name the item and field and nothing is applied. Sizes may be a number, a short array or {radius, height}; colours [r,g,b], [r,g,b,a], 0..255 or \"#rgb\" / \"#rrggbb\"; material input defaults to its name (in place).
- SVG artwork: material node svg {svg: markup | path, size, center, angle, fit} is an exact SDF of the drawing (ports: default union, one per element id \"logo.ring\", logo.colors its own fill colours, logo.alpha); project {face front|back|left|right|top|bottom or normal, at, depth} gives planar coords (at = [0, 0], so give the artwork center [0, 0]) and project.facing to put it on one side of a part in the object domain (mask by facing). Any profile (extrude, revolve, sweep, loft sections, silhouette) also takes {svg, size | height, center, flip_y, id}: rings with holes.
- Every model gets materials. Shape first, materials after: build and check the geometry, then make the materials (material_ops) and give each visible part a material item in a later apply_ops, preferably a procedural pbr one ({\"type\": \"material\", \"name\": part, \"pbr\": name}; the material must exist when the item is applied); a part with only a flat colour is a warning until it gets a pbr material, or \"flat\": true on its material item when the user wants a plain colour. Check with verify/expect {\"materials\": true} (no default-grey parts) or \"pbr\" (no flat colours); save_prefab warns about grey parts.
- Texture by default, even when the user only asks for a shape: a model is not finished until every visible part has a real surface. Plan the surfaces with the shape: tile pbr materials (wood grain, brushed metal, painted steel, fabric, stone, plastic with wear) for anything that repeats or is reused; an object-domain material for hero parts (mesh_ao / mesh_curvature for dirt in creases and edge wear, macro / bevel finishes); svg or text nodes on a project {face, at} for badges, labels, panels, markings; decals for stains and graffiti. Give painted parts edge wear and grime, not one flat tone. Check with material_view and an agent_views render; verify materials: \"pbr\". Only skip it when the user asks for a blockout or a plain colour (flat: true).
- Materials follow lineage: copies and cuts of a part inherit its in-place material wherever the material item sits.
- A timeout error from your MCP client does not mean the call failed: the server keeps working, so check history or scene_summary before retrying an apply_ops.
- Several agents may share one server (the driver daemon): each keeps its own active document, results say what other agents changed in yours since your last call, undo refuses to revert another agent's step (undo force: true), apply_ops warns when it replaces another agent's op, and save_prefab refuses a catalog file someone else saved since you loaded it (force: true to overwrite). Agents that share a session (subagents share one client id) must pass doc: <open document name> on every tool that reads or changes a document (apply_ops, verify, inspect, agent_views, scene_summary, ops, op_get, probe_ray, screenshot, set_camera, style, material_ops, annotations, history, undo, redo, jump, diff, cherry_pick, history_prune, set_model_name, save_prefab, export_model); a doc that was idle-closed, parked or is a catalog prefab is reopened automatically (unknown names error), and save_prefab refuses a name another open document holds. new_model never displaces work: unless the current document is empty it starts beside it (both stay open), and a name that is already open is refused.
- Working efficiently: verify numerically between steps and render once per model at the end with one tiled agent_views (e.g. [\"front\", \"right\", \"top\", \"iso\"]), not after every step. Pictures cost tokens (about w x h / 750 each, and every call says what it spent): views are sized to the model and cheap by default (detail \"low\"); ask for detail \"high\" or inspect image: true only when fine detail is the question, and prefer verify / inspect numbers, which cost almost nothing. For many identical props in a map (hundreds of desk fans), model one prefab (the fan, with its rotor clip) and place prefab instances, or bake a set into one prefab with arrays: never hundreds of separate items. Prefab instances export by reference in a .bsn.
- transform is absolute unless relative: true (offsets: translation adds, rotation composes, scale multiplies).
- radial_array defaults to include_source: true (source = item 0, count items evenly spread); axis x/z for horizontal rings.
- extrude (footprint [[x, z], ...] up y = 0..height; twist, scale_top about [0, 0]) and revolve ([[radius, y], ...] around y; degrees) make closed solids from 2D profiles; inner rings are holes. Profile entries may be arcs {\"arc\": {center, radius, from, to}}; round: r fillets profile corners.
- sweep runs a profile along a 3D path (pipes, rails, handles; closed for rings; smooth: true makes the path a curve through a few control points, twist / scale_end taper it); loft skins a closed solid through sections that change shape and size ({sections: [{profile, y | at, scale?, rotation?}, ...], smooth: true} for a bottle, circle into square, blade; with a path, sections sit at t along it; scale 0 at an end makes a point); path_array copies a part along a path (name_0.., count or spacing, align: +x along the path, roll_step 90 for chain links; hide the source); lsystem grows branching things (trees, bushes, ferns, roots, coral) from a preset (tree/pine/bush/fern/weed/roots, vary seed) or axiom + rules (parametric too: \"A(l, w) : l > 0.1\": \"!(w)F(l)[&(40)A(l*0.6, w*0.7)]A(l*0.9, w)\"), leaf_angle/curl/fold/stalk, wood in name and leaves in name_leaves; leaves are cards by default (flat quads with uvs 0..1, leaf_cross for a crossed pair): give name_leaves a material_ops material with domain \"card\" whose output sets opacity in a leaf shape (an alpha cutout; AGENTS.md has a leaf), or several leaves per image with a bigger leaf_size for dense crowns; leaf_mode \"solid\" models closed blades in leaf_shape instead; terrain makes level ground: a closed noise-heightfield slab at real size (style hills/ridges/dunes/mesa/flat, size, height, scale, seed; flatten pads [{at: [x, z], radius}] level building plots, paths [{points, width, depth}] carve roads and rivers; place things onto it); scatter strews copies over a surface ({input: part | [parts] | [[set]] | \"inst.*\", on, count | density, seed, scale [min, max], rotate random_y/random/none, align 0..1, min_spacing, max_slope, region}: rocks, forests of lsystem trees; copies name_0.. sit on the surface, hide the source); hull = convex hull of parts; blob melts solids together with a smooth fillet (blend meters; op union/subtract/intersect; creatures, mushrooms); split cuts a solid by a plane {normal, point} into capped name_above / name_below (gap pushes them apart); round fillets/chamfers/smooths a solid's edges; deform reshapes a mesh: displace (noise along normals: amount, scale, seed, noise fbm/ridged = rocks, cliffs), twist/bend (degrees along axis, bend toward a direction) and taper (scale_end): horns = taper then bend, material (pbr: a procedural material's height becomes real relief: bark, scales, bricks); sdf models organic solids from analytic shapes (sphere, ellipsoid, box, capsule = rounded cone, cylinder, torus, chain of [x, y, z, r], part, group) joined in order by smooth union/subtract/intersect (blend), mirror: \"x\" to model one side, warps (twist/bend/taper/displace) on the whole field, plus local sculpt warps on sdf / skin / body ({mode: inflate (amount m) | push (direction, amount) | pinch (0..1 toward at) | flatten (normal, 0..1 to the plane through at) | smooth (local blur), at: point or anchor (a joint name such as \"head\", \"part:top\"), radius, falloff}; zero effect outside radius; they never fold: push / pinch / flatten are capped (push about 0.53 x radius), so use a larger radius or several warps for more; under the item mirror (and always on a body, about x = 0) they are symmetric unless the warp says mirror: false; smooth takes size, the blur reach): creatures, mushrooms, bones; silhouette {side [[z, y]], top [[x, z]], front? [[x, y]], smooth, round, blend} carves a solid from orthographic outlines (intersected extrusions, edges rounded by blend; a car body, a shoe, a gun), also an sdf shape {shape: \"silhouette\", side, top, front, round, at}; from_reference {image, view, height | size, flip?, regions?, holes?} traces an outline from a project PNG (front from +z, side / right from +x with image right = -z, left from -x, top = plan; a changed PNG re-traces on the next re-derive); skin wraps a skeleton (chains of [x, y, z, radius] or joint names) in one smooth surface: tentacles, roots, hands; chain / skin points may be ovals [x, y, z, [rx, ry], roll?] (rx sideways, ry along the tube's up, roll degrees; a sixth entry n makes a superellipse: 2 ellipse, 3-5 rounded box) so torsos, snouts and tails aren't round tubes; sdf / skin / body take tris: N (and bake) to decimate in the same item (no sdf + decimate + hide triple); body starts a creature or vehicle from a preset (biped 1.75 m, quadruped = a cat 0.25 m at the shoulder, bird, fish, serpent, car; preset \"human\" is a CC0 template mesh with morph targets instead: gender 0..1, age (years), muscle, weight, height (m), shape {detail target: -1..1}, expression {unit: 0..1}, shapes {name: {expression, shape}} = glTF morph targets, detail 0..2 subdivision, extras <name>_eyes / _teeth / _tongue, hair: \"bob\" | {style (bald buzz short crop bob long ponytail bun afro curly), color, part, fringe, volume} making <name>_hair + _hair_cap, brows / lashes cards on by default, skin_tone 0..1 or a colour, landmark anchors (\"eve:nose_tip\", \"eve:brow_l\", \"eve:mouth_corner_l\") and a material node zone {zone: lips | nails | cheeks | ...} to paint by region, a 58-bone biped-named rig with the template's own weights) sized by height, head_ratio, build, leg_length, tail, snout and per-joint {scale, offset} edits, standing on y = 0 facing +z: it brings a <name>_rig skeleton with the same joint names and an implicit smooth <name>_bind (a bind item on that rig replaces it; bind: false turns it off), and those names are anchors (\"cat:paw_l\") for move pivots, frames and skeleton heads; smooth relaxes (Taubin, no shrink), remeshes (even triangles of edge m) or subdivides (Catmull-Clark of a coarse cage, crease degrees keep edges); decimate lightens a dense part (ratio or tris; non-destructive: the input stays in the graph, hide it) and with bake: true writes the dense surface into the light part's normal and height maps; check organic parts with verify self_intersections / thickness, inspect thickness and agent_views style \"curvature\"; group merges disjoint parts into one (array or move a set; use it instead of unioning parts that don't touch).
- Booleans run on Manifold: closed outputs, exact coplanar and curved cuts. Both inputs must be closed solids (disc/plane/washer error).
- params {\"h\": 2} plus values \"$h\" or \"=$h/2 + 0.1\": changing a param later re-derives every op that reads it, in one undo step.
- prefab items instance a catalog model; its visible parts become name.part.
- Verify in the same call with apply_ops expect {parts (\"gz.*\" globs), includes, grounded, supported, rests [[a, b]], attached [[a, b]] (touching on any side, no gravity: mechanisms), collisions, stable, bounds {min,max,tol}, validate, rebuild, scope [patterns], coaxial / centered_on [[a, b]], flush [{a, b, face}], symmetric {parts, axis}, equal_spacing {parts}, density {min_features_per_m2, min_area} (big bare surfaces)}, or call verify. Warnings also flag near misses (1-5 mm gaps / off-flush faces, near-symmetry, uneven runs, sizes under 2% apart) and boxy models (style number lint_boxy: 0 silences that). grounded = reaches the ground through touching parts (real geometry); supported = nothing hangs (held through contact below the centre of mass or a joint by parts that are themselves supported); collisions = no part passes through another (grounded/supported count overlaps as joints; grounded names parts held up only by a deep overlap, \"strict\" fails them). A collisions allow entry may be [a, b, depth] or {\"pair\": [a, b], \"depth\": 0.003} to excuse only shallow overlaps; avoid [x, \"*\"] catch-alls. move/place part globs match visible parts only (exact names and a bare * still take hidden ones); move/place results report where the parts went, parts left behind and new overlaps. probe_ray proves openings (headless too). walkable {from, to, via?, agent?: {radius 0.3, height 1.8, step 0.3, max_slope 45}} checks a character can walk the route (stairs, doorways, corridors) and names the blocker: a step too tall, low headroom, a gap too narrow, a steep slope.
- Interiors by subtraction: {\"type\": \"carve\", \"name\": \"bunker\", \"cutters\": [\"hall\", \"cell\", \"door\"], \"split_by\": \"facing\", \"markers\": true, \"collider\": \"mesh\"} takes closed cutter parts (rooms, halls, doorways: boxes through the wall; hide them) out of a mass (solid, or omit it: a box around the cutters, wall 0.3 m thick) and keeps only the faces looking into the rooms, as merged one-sided sheets (no hidden outer faces). rooms [{name, footprint [[x, z]..], height, base}] make room cutters from outlines. split_by none|facing|cutter|both gives name, name_floor/_wall/_ceiling, name_<cutter> (wearing the cutter's material) or name_<cutter>_<facing>; keep outer|both adds name_outer; markers makes room / opening markers; collider mesh gives the sheets trimesh colliders; collider blocks cuts the carved mass into box `block` markers (Block component) for games that build colliders from markers. Cut first (the sheets are open and can't feed booleans), then materials, scatter, lights; check with verify walkable. Workflow in AGENTS.md (Interior workflow).
- Organic workflow: start from body or silhouette (or sdf / skin with oval sections), compare with agent_views reference in up to 3 views and read the band deviations, fix them with local warps or joint overrides, check style \"curvature\" and inspect proportions, then set tris (+ bake) last. Level-set results are finished automatically (sharp joins come out straight); give a join meant to be soft a blend, and treat a ragged crease warning as a cue to re-derive or raise tris.
- inspect answers \"how close / how thick / will it stand\": measure [[a, b]] (gap, touching, or overlap volume and depth), section {axis, at} (cut outlines, areas, thinnest wall, plus an image of the cut) and mass (centre of mass, tips over or not); proportions {parts?} gives a body's bone-chain lengths and ratios (biped head/height ~0.133, leg/height, arm span/height).
- agent_views renders named views or cameras you place ({yaw, pitch, dist?, target?, ortho?}; yaw 0 = front) and tiles several into ONE image sized to the model (detail \"low\", about 800 tokens for four views; \"high\" for fine detail; never over ~1.15 MP per call): [\"front\", \"right\", \"top\", \"iso\"] is one read. style \"ids\" = flat colour per part with a legend; dims = sizes + scale bar; diff = what changed since the last render of the same views (cropped to the change; no picture when nothing moved); style \"silhouette\" (black on white: shape only) and \"density\" (red = sparse, green = busy); focus \"joints\" (or joints: N) frames the contact regions as close-ups; reference {image, view} (or a list: front, side, top in one call) compares the silhouette with a PNG: overlay, IoU and the biggest band deviations (\"front: top 0-20%: model 14% wider (+3.1 cm)\"). Look at an image after building.
- scatter places clutter where it belongs: near [parts] + near_distance (3D distance to their surfaces; along wall bases), edge_bias (corners, borders), clump {count, radius}, mask path / pad / not_path / not_pad and height [min, max] on terrain; scatter with decal {pbr, size} instead of input strews decals (cracks, stains, leaves).
- apply_ops results end with a brief summary (parts added/changed/removed + warnings); summary: \"full\" or scene_summary for everything. Each apply_ops call is one undo step; history shows the newest 30 nodes (limit for more).
- undo/redo/jump move a history-tree cursor; jump restores that node's ops exactly. diff(from, to?) shows what changed; cherry_pick(from, ops?) replays ops from another branch.
- When a call surprises you (an error that didn't say what to do, a field that didn't work as described, a workaround you needed), tell the maintainers in one or two sentences with session_log(feedback: \"...\"): what you tried, what happened, what you expected. Failed and slow calls are logged on their own; notes say why.
- scene_summary, apply_ops and verify also return structuredContent JSON (apply_ops: the result text plus counts; summary \"full\" for every part). material items take name patterns (\"wall_*\"), which keep matching parts later batches add; shrinking an array drops the edits of its lost copies (a note lists them); a material_ops edit re-applies itself to the parts using it. catalog_describe(name, delete: true) removes a prefab.
- Cosmetic variants (skins: red/blue team, clean/rusty): {\"type\": \"variant\", \"name\": \"red_team\", \"materials\": {\"body*\": {\"pbr\": \"paint\", \"params\": {..}}, \"trim\": {\"color\": \"#c33\", \"flat\": true}}, \"style\"?: {token patch}} overrides only the matched parts' materials (geometry never changes; the base look is \"default\"). See them with agent_views variant / variants: true, set_camera variant (window); verify materials checks every variant; exports write KHR_materials_variants plus node extras.variants (modeler_bevy's ModelerVariant swaps them in game); a prefab item takes variant.
- collider rules (parts globs, shape box/hull/mesh/none, body, sensor) and marker items (spawns, triggers with size, data JSON) export as node extras for game code; they ride prefabs and moves.
- light items (point/spot/directional, Bevy units: lumens/lux, attach to a part) export as KHR_lights_punctual; agent_views lighting: \"scene\" renders with only the model's lights.
- Animation: rig after the shape and materials. skeleton (bones {name: {head, tail, parent, radius}}: points or anchors \"leg_l:top\", world-aligned rest unless a bone sets rest (a rotation) or roll (degrees about head -> tail, +y along the bone): then its clip rotations turn about its local axes, mirror \"x\" makes *_r twins; or from: an lsystem / skin op to auto-rig; a bone may set limits {x: [min, max], y, z} (degrees, Euler x then y then z of its clip rotation, clamped after tracks and IK) and look_at {target: point | anchor | bone, axis?, weight?} for eyes and heads), bind (rigid with bone, smooth weights, auto = exact for lsystem rigs: bind [\"oak\", \"oak_leaves\"]), clip (tracks per bone: rotate/translate/scale keys [[t, value]] as offsets from rest, or {wave}; mirror {leg_r: {from: leg_l, phase: 0.5}} half-authors a walk; ik {ankle_l: {target: [x, y, z] | anchor | keys [[t, [x, y, z]]], chain: 2, pole?, weight?: 0..1 or keys, rotate?: keys}} drives the named bone's HEAD (its joint) to a world target by bending the chain bones above it, after the tracks: plant feet and reach hands by target instead of keying angles (body bipeds: ankle_l / ankle_r for feet, wrist_l / wrist_r for hands); mirror copies ik entries too, target x negated; rotate {euler: [[0, {y: 0}], [1, {y: 360}]]} keys cumulative degrees for spins; poses [[0, \"rest\"], [0.4, \"crouch\"]] keys whole pose items (pose {skeleton, bones {bone: {rotate, translate, scale}}, mirror?, ik? solved once}; explicit tracks override per channel; agent_views / inspect / verify take a pose name as a clip); over: \"walk\" layers on a keyed base (additive? composes, else replaces; weight 0..1 or keys; bones globs; duration defaults to the base's), then limits, IK (base first) and look_at run once; root_motion: true | {bone, axes \"xz\", yaw?} plays travelling here but exports in place with extras.root_motion {delta, samples} for the game; events [[t, name, data?]] export to extras.events; morphs {part: {shape: keys [[t, w]] | number | {wave: {amount, period}}}} keys morph target weights (a human body's shapes; part = its name), applied before skinning, layered by over (additive adds), also in pose items (morphs {eve: {smile: 1}}) and exported as glTF weights channels), sway (wind: direction, strength, gust, flutter; spring over: a clip for tails and ears; bakes into a looping clip). mocap {skeleton, file: \"mocap/walk.bvh\" (a BVH in the project; a changed file re-derives), map?: \"auto\" | {our bone: BVH joint}, range?, fps?, loop?: true | \"blend\" + blend s, root_motion?: \"in_place\" (default; speed kept on the clip) | true, feet?: {plant, contact_height, contact_speed}, mirror?, speed?, facing?: deg | \"auto\"} imports motion capture as an ordinary clip: names mapped automatically (CMU / Motionbuilder, Mixamo, 3ds Max biped, fingers), retargeted in world space from the file's rest pose to the rig's (bones point where the source bones point), root scaled by leg length, feet planted with IK; the apply result reports the mapping, units, contacts, slide and loop seam. Prefer it to hand-keyed clips for people. Judge motion with an agent_views filmstrip (clip + times [0, 0.25, 0.5, 0.75], style bones / weights) , agent_views style \"trails\" + clip (the path each bone tip travels over the clip, dots at even time steps, planted stretches in white; bones globs) and inspect curves {clip, bones?, channels?: [height, speed, angle, x, z]} (per-bone min / max, planted spans and one plot image, headless), and verify animation {clip} (loop incl. seam velocity, floor, collisions, stretch, foot_slide {bones?, speed?}, balance, smooth, gait: a biped walk against normative human gait kinematics (thigh / knee / ankle curves by percent of the cycle from heel strike, stance share, cadence, stride, pelvis bob, arm phase, toe clearance; each failure names the value, phase and normal value); build human walks from measured references (examples/gait_gen.py), not by eye) plus weights: true; export writes skins and animations, and crates/modeler_bevy runs the sway live in a game.
- scene(open/switch/list/close) keeps several documents open; a prefab item naming an open doc instances it live (re-synced on switch); prefab style: \"scene\" re-themes a piece with the scene's tokens.
- Every mutation autosaves the session; save_prefab names and indexes it (model name from set_model_name). export_model skips hidden parts.
- Render waits time out after 15s with an error.
- Style tokens (a cohesive look): set them first with style(action \"set\" or \"load\"), then build with \"@primary\" colours (.darken(x) .lighten(x) .mix(@b, x) .alpha(x)), \"$bevel\" numbers, \"style\": \"<preset>\" on material items; style defaults fill omitted fields. Prefer tokens over literal colours; follow the style's brief. Changing a token re-derives every item reading it.
- Procedural PBR materials: material_ops(material, ops) builds a node graph (coords, 2D/3D SDFs, noise, worley, math, colour; one output node with base_color, roughness, metallic (metalness works), height (m), ao...; card / decal space is y-down, v 0 = top), atomic like apply_ops, with expect {tileable, ranges, albedo, normals, coverage}. describe_op(\"nodes\") lists node types, \"node:<type>\" one. material_view shows a lit sphere + channels (or one node). material_catalog saves/loads the library. Put one on a part with an apply_ops material item: {\"type\": \"material\", \"name\": \"wall\", \"pbr\": \"bricks\", \"params\"?: {..}, \"scale\"?: 1.5} (tile materials map at real size and are shared: a .gltf export (the default, preferred over glb) writes each texture once into assets/textures/ as GPU-compressed mipmapped KTX2, named by content, so every model reusing the material points at one file; object materials wrap a per-part atlas no other model can share, so prefer tile for kit materials). undo/redo/jump/history take doc \"material:<name>\".
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
/// Tools that take a `doc` argument naming an open document.
const DOC_TOOLS: &[&str] = &[
    "apply_ops",
    "verify",
    "inspect",
    "agent_views",
    "save_prefab",
    "undo",
    "redo",
    "export_model",
    "history_prune",
    "scene_summary",
    "ops",
    "op_get",
    "history",
    "jump",
    "diff",
    "cherry_pick",
    "screenshot",
    "set_camera",
    "material_ops",
    "set_model_name",
    "style",
    "add_annotation",
    "list_annotations",
    "delete_annotation",
    "probe_ray",
];

fn material_doc(doc: Option<&str>) -> Result<Option<Option<&str>>, String> {
    match doc {
        None | Some("model") => Ok(None),
        Some("material") => Ok(Some(None)),
        Some(d) => match d.strip_prefix("material:") {
            Some(n) => Ok(Some(Some(n))),
            // An open document's name: call_tool already switched to it.
            None => Ok(None),
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
