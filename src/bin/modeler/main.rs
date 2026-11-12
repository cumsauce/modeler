//! The modeler preview app: a small Bevy window showing the model, driven by an MCP
//! server on stdio. No game code.
//!
//! Run `modeler` for a windowed preview, `modeler --headless` for no window/render,
//! `modeler --model <path.json>` to start from a saved [`modeler::Model`], and
//! `modeler --restore` to pick up the autosaved session (the default starts empty).
//!
//! Bridge design: the MCP server runs in its own thread on its own tokio runtime
//! (winit wants the main thread, so Bevy keeps it). Tools send a `Cmd` (a closure over
//! the [`Session`] or a screenshot request) plus a oneshot reply channel over a tokio
//! mpsc; a Bevy system drains the channel every frame, executes against the session
//! resource and answers.

mod prelude;
mod preview;
mod server;

use prelude::*;

/// Why the MCP side stopped (stdin EOF, transport error), set by its thread.
static MCP_STOP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
/// The last panic's message, for the exit line.
static LAST_PANIC: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// One line on stderr saying why the server exits. The daemon in
/// `mcp_driver.py` greps for this prefix and quotes it in its restart note.
fn log_exit(reason: &str) {
    eprintln!("modeler: exit: {reason}");
}

/// Logs every panic with its thread and location (then the default report),
/// and remembers it for the exit line.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string payload".into());
        let at = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let thread = thread::current();
        let line = format!(
            "panic in thread '{}': {msg}{at}",
            thread.name().unwrap_or("?")
        );
        eprintln!("modeler: {line}");
        if let Ok(mut g) = LAST_PANIC.lock() {
            *g = Some(line);
        }
        default(info);
    }));
}

fn main() -> AppExit {
    let mut headless = false;
    let mut offscreen = false;
    let mut print_catalog_dir = false;
    let mut model_path: Option<PathBuf> = None;
    // Start empty unless asked: `--restore` (or MODELER_RESTORE=1) brings
    // back the autosaved session, open docs and working materials.
    let mut restore = std::env::var("MODELER_RESTORE").is_ok_and(|v| v == "1");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headless" => headless = true,
            "--restore" => restore = true,
            "--offscreen" => offscreen = true,
            "--model" => model_path = args.next().map(PathBuf::from),
            "--print-catalog-dir" => print_catalog_dir = true,
            "--feedback-report" => {
                // The friction log agents left (failures, slow calls, notes).
                println!(
                    "{}",
                    modeler::feedback::report(&modeler::feedback::read(), None)
                );
                return AppExit::Success;
            }
            "--split-history" => {
                // Prefab files saved before history sidecars: move it out.
                match modeler::catalog::split_prefab_histories() {
                    Ok((n, before, after)) => println!(
                        "moved the history out of {n} prefab file(s) into git-ignored *.history.json sidecars: {:.1} MB -> {:.1} MB tracked",
                        before as f64 / 1e6,
                        after as f64 / 1e6
                    ),
                    Err(e) => {
                        eprintln!("split-history failed: {e}");
                        return AppExit::error();
                    }
                }
                return AppExit::Success;
            }
            "--project" => {
                let Some(dir) = args.next() else {
                    eprintln!("--project needs a directory");
                    std::process::exit(2);
                };
                let dir = std::fs::canonicalize(&dir).unwrap_or_else(|_| PathBuf::from(&dir));
                // SAFETY: set before any other thread starts.
                unsafe { std::env::set_var("MODELER_PROJECT", dir) };
            }
            other => eprintln!(
                "unknown argument `{other}` (usage: modeler [--headless | --offscreen] [--model <path.json> | --restore] [--project <game dir>] [--print-catalog-dir] [--feedback-report] [--split-history])"
            ),
        }
    }
    if print_catalog_dir {
        // For mcp_driver.py: where the autosave (and daemon.log) live.
        return match catalog::catalog_dir() {
            Ok(dir) => {
                println!("{}", dir.display());
                AppExit::Success
            }
            Err(e) => {
                eprintln!("modeler: {e}");
                AppExit::error()
            }
        };
    }
    install_panic_hook();

    // One autosaving server per catalog dir: a second live server would
    // clobber the first's session.json, so it runs without autosave.
    match catalog::acquire_session_lock() {
        Ok(catalog::LockOutcome::Owner) => {}
        Ok(catalog::LockOutcome::HeldBy(pid)) => eprintln!(
            "modeler: autosave SLOT {}: server pid {pid} holds the session lock in this catalog dir, \
             so this server autosaves to session.{}.json instead",
            catalog::autosave_slot(),
            catalog::autosave_slot()
        ),
        Err(e) => eprintln!("modeler: could not take the session lock: {e}"),
    }

    let mut session = Session::default();
    if let Some(path) = &model_path {
        let loaded = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| {
                serde_json::from_str::<Model>(&s)
                    .map_err(|e| e.to_string())
                    .and_then(|m| m.check_format().map(|_| m).map_err(|e| e.to_string()))
            });
        match loaded {
            Ok(model) => {
                eprintln!("loaded model `{}` from {}", model.name, path.display());
                session.model = model;
            }
            Err(e) => eprintln!("could not load {}: {e}", path.display()),
        }
    } else if !restore {
        // Starting empty: keep the last autosave as *.prev.* so this
        // server's first edit can't overwrite it (`--restore` reclaims it).
        let n = catalog::keep_previous_session();
        if n > 0 {
            eprintln!(
                "kept the previous autosave as {n} *.prev.* file(s); `--restore` brings it back"
            );
        }
    } else {
        let n = catalog::reclaim_previous_session();
        if n > 0 {
            eprintln!("reclaimed the previous autosave from {n} *.prev.* file(s)");
        }
        // Restore the last session's autosave so unnamed WIP (ops, parts,
        // history tree, annotations) survives a restart.
        if let Some(model) = catalog::load_session_state() {
            eprintln!(
                "restored session model `{}` (v{}) from autosave",
                model.name, model.version
            );
            session.model = model;
        }
        let active = session.model.name.clone();
        session.others = catalog::load_open_docs()
            .into_iter()
            .filter(|m| m.name != active)
            .collect();
        // Only unsaved work comes back: documents that match their
        // catalog file stay closed (they reopen from the catalog).
        let closed = session.close_saved_documents();
        if !closed.is_empty() {
            eprintln!(
                "left {} saved document(s) closed: {}",
                closed.len(),
                closed.join(", ")
            );
        }
        session.register_docs();
        if !session.others.is_empty() {
            eprintln!("restored {} unsaved open document(s)", session.others.len());
        }
        session.materials = modeler::material::Library::restore();
        if !session.materials.docs.is_empty() {
            eprintln!(
                "restored {} working material(s) from autosave",
                session.materials.docs.len()
            );
        }
    }

    let (tx, rx) = mpsc::channel::<Cmd>(64);
    let bridge = Arc::new(Bridge {
        rx: Mutex::new(rx),
        server_done: AtomicBool::new(false),
    });

    // The MCP server gets a thread of its own; winit/render must own the main thread.
    {
        let done = Arc::clone(&bridge);
        thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("modeler: could not start MCP runtime: {e}");
                    done.server_done.store(true, Ordering::Relaxed);
                    return;
                }
            };
            rt.block_on(async move {
                let server = match Tools::new(tx).serve(rmcp::transport::stdio()).await {
                    Ok(server) => server,
                    Err(e) => {
                        eprintln!("modeler: MCP server failed to start: {e}");
                        done.server_done.store(true, Ordering::Relaxed);
                        return;
                    }
                };
                let why = match server.waiting().await {
                    Ok(_) => "stdin EOF (MCP client gone)".to_string(),
                    Err(e) => format!("MCP server stopped: {e}"),
                };
                eprintln!("modeler: {why}");
                let _ = MCP_STOP.set(why);
                done.server_done.store(true, Ordering::Relaxed);
            });
        });
    }

    let mut app = App::new();
    app.insert_resource(Modeler {
        session,
        bridge: Arc::clone(&bridge),
    })
    .insert_resource(Headless(headless))
    .insert_resource(Offscreen(offscreen && !headless))
    .insert_resource(PendingShots::default())
    .insert_resource(PendingAgentShots::default())
    .insert_resource(AgentRig::default())
    .insert_resource(Orbit::default())
    .init_resource::<CameraDrag>()
    .init_resource::<SceneBuilt>()
    .init_resource::<PbrCache>()
    // The material graph shows in Material mode; the left pane in a document.
    .insert_resource(MatPane::open(false))
    .insert_resource(OutlinePane::open(!offscreen))
    // The grid is the default view in the window (not offscreen, where
    // `screenshot` renders the window camera); MODELER_GRID=0 starts without.
    .insert_resource(Grid::open(
        !offscreen && std::env::var("MODELER_GRID").map_or(true, |v| v != "0"),
    ))
    .init_resource::<Recency>()
    .init_resource::<PinTyping>()
    .init_resource::<Playback>()
    .init_resource::<CommentBox>()
    .init_resource::<PendingPinSpawn>()
    .add_systems(
        Update,
        (
            drain_commands,
            quit_when_server_done(headless || offscreen),
            autosave_tick,
            take_screenshot.run_if(windowed),
            // The grid covers the window's own view: rebuild it only when
            // it shows (parallel agents switch the active document per call).
            track_recency.run_if(windowed),
            rebuild_scene.run_if(windowed).run_if(scene_wanted),
            (playback_keys, play_animation)
                .chain()
                .after(rebuild_scene)
                .run_if(windowed),
            draw_game_gizmos.run_if(windowed),
            update_mode.run_if(windowed),
            orbit_camera.run_if(windowed).run_if(grid_off),
            (click_in_view, spawn_pin_markers, sync_pins)
                .chain()
                .after(rebuild_scene)
                .run_if(windowed)
                .run_if(grid_off),
            comment_box.run_if(windowed),
            update_typing_hud.run_if(windowed),
            highlight_selected.run_if(windowed),
            update_material_pane.run_if(windowed),
            update_outline_pane.run_if(windowed),
        ),
    );
    app.add_systems(Last, autosave_on_exit);
    if headless {
        app.add_plugins(MinimalPlugins); // MinimalPlugins already runs the schedule loop
    } else {
        // Synchronous pipeline compilation: a frame that needs a new shader
        // variant waits for it instead of skipping those meshes, so a render
        // right after startup (or after a new material kind appears) never
        // comes back empty or missing parts. Costs a short stall, once.
        let plugins = DefaultPlugins.set(bevy::render::RenderPlugin {
            synchronous_pipeline_compilation: true,
            ..default()
        });
        if offscreen {
            // Full renderer, no window: no winit, so a runner drives frames.
            app.add_plugins(
                plugins
                    .set(WindowPlugin {
                        primary_window: None,
                        exit_condition: bevy::window::ExitCondition::DontExit,
                        close_when_requested: false,
                        ..default()
                    })
                    .disable::<bevy::winit::WinitPlugin>(),
            )
            .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
                std::time::Duration::from_secs_f64(1.0 / 60.0),
            ));
        } else {
            app.add_plugins(plugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "modeler".into(),
                    ..default()
                }),
                ..default()
            }));
        }
        app
            // Deferred by default so screen-space reflections work; cameras
            // without a deferred prepass (the window) still render forward.
            .insert_resource(bevy::pbr::DefaultOpaqueRendererMethod::deferred())
            .add_systems(
                Startup,
                (
                    setup_scene,
                    setup_material_pane,
                    crate::server::install_wake,
                ),
            )
            .add_systems(Startup, setup_agent_camera)
            .add_plugins(bevy::feathers::FeathersPlugins)
            .insert_resource(bevy::feathers::theme::UiTheme(
                bevy::feathers::dark_theme::create_dark_theme(),
            ))
            .add_systems(Startup, setup_outline_pane)
            .add_systems(Update, focus_on_new_agent)
            // The workbench and its panes (each file registers its own systems).
            .add_plugins((
                workbench_plugin,
                top_bar_plugin,
                doc_popup_plugin,
                right_pane_plugin,
                timeline_plugin,
                minimap_plugin,
                shading_plugin,
            ))
            .add_systems(
                Update,
                (toggle_grid_key, update_grid.after(apply_layout)).run_if(windowed),
            )
            .add_systems(
                Update,
                (process_agent_shot.run_if(windowed).after(rebuild_scene),),
            );
    }
    // Catch a main-thread panic so the exit line and lock cleanup still run.
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.run()));
    catalog::release_session_lock();
    match run {
        Ok(exit) => {
            let mcp = MCP_STOP.get().cloned();
            let reason = match (headless || offscreen, mcp) {
                (true, Some(why)) => why,
                (false, Some(why)) => format!("window closed (after {why})"),
                (false, None) => "window closed".to_string(),
                (true, None) => "app exited".to_string(),
            };
            let reason = match &exit {
                AppExit::Success => reason,
                AppExit::Error(code) => format!("{reason}, error code {code}"),
            };
            log_exit(&reason);
            exit
        }
        Err(_) => {
            let what = LAST_PANIC
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_else(|| "panic".into());
            log_exit(&what);
            AppExit::from_code(101)
        }
    }
}
