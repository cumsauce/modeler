//! The modeler preview app: a small Bevy window showing the model, driven by an MCP
//! server on stdio. No game code.
//!
//! Run `modeler` for a windowed preview, `modeler --headless` for no window/render,
//! and `modeler --model <path.json>` to start from a saved [`modeler::Model`].
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

fn main() -> AppExit {
    let mut headless = false;
    let mut model_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headless" => headless = true,
            "--model" => model_path = args.next().map(PathBuf::from),
            other => eprintln!(
                "unknown argument `{other}` (usage: modeler [--headless] [--model <path.json>])"
            ),
        }
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
    } else {
        // No explicit model: restore the last session's autosave so unnamed
        // WIP (ops, parts, history tree, annotations) survives a restart.
        if let Some(model) = catalog::load_session_state() {
            eprintln!(
                "restored session model `{}` (v{}) from autosave",
                model.name, model.version
            );
            session.model = model;
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
                if let Err(e) = server.waiting().await {
                    eprintln!("modeler: MCP server stopped: {e}");
                }
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
    .insert_resource(PendingShots::default())
    .insert_resource(PendingAgentShots::default())
    .insert_resource(AgentRig::default())
    .insert_resource(Orbit::default())
    .init_resource::<CameraDrag>()
    .insert_resource(GraphHudVisible(std::env::var_os("MODELER_GRAPH").is_some()))
    .init_resource::<GraphPanel>()
    .init_resource::<SceneBuilt>()
    .init_resource::<PbrCache>()
    .insert_resource(MatPane::open(
        std::env::var_os("MODELER_MATERIALS").is_some(),
    ))
    .init_resource::<PinTyping>()
    .init_resource::<PendingPinSpawn>()
    .add_systems(
        Update,
        (
            drain_commands,
            quit_when_server_done(headless),
            take_screenshot.run_if(windowed),
            rebuild_scene.run_if(windowed),
            orbit_camera.run_if(windowed),
            pin_on_click.run_if(windowed),
            spawn_pin_markers.run_if(windowed),
            pin_typing.run_if(windowed),
            update_typing_hud.run_if(windowed),
            update_graph_hud.run_if(windowed),
            highlight_selected.run_if(windowed),
            graph_pane_viewport.run_if(windowed),
            update_material_pane.run_if(windowed),
        ),
    );
    if headless {
        app.add_plugins(MinimalPlugins); // MinimalPlugins already runs the schedule loop
    } else {
        // Synchronous pipeline compilation: a frame that needs a new shader
        // variant waits for it instead of skipping those meshes, so a render
        // right after startup (or after a new material kind appears) never
        // comes back empty or missing parts. Costs a short stall, once.
        app.add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "modeler".into(),
                        ..default()
                    }),
                    ..default()
                })
                .set(bevy::render::RenderPlugin {
                    synchronous_pipeline_compilation: true,
                    ..default()
                }),
        )
        // Deferred by default so screen-space reflections work; cameras
        // without a deferred prepass (the window) still render forward.
        .insert_resource(bevy::pbr::DefaultOpaqueRendererMethod::deferred())
        .add_systems(Startup, (setup_scene, setup_material_pane))
        .add_systems(Startup, setup_agent_camera)
        .add_plugins(bevy::feathers::FeathersPlugins)
        .insert_resource(bevy::feathers::theme::UiTheme(
            bevy::feathers::dark_theme::create_dark_theme(),
        ))
        .add_systems(Startup, setup_toolbar)
        .add_systems(Update, sync_toolbar)
        .add_systems(Update, (process_agent_shot.run_if(windowed),));
    }
    app.run()
}
