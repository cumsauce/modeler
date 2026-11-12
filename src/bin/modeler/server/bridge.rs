use crate::prelude::*;

pub(crate) type CmdResult = modeler::commands::CmdResult;
pub(crate) type ShotReply = oneshot::Sender<Result<Vec<u8>, String>>;

/// A unit of work for the Bevy main loop: mutate the session, or grab a picture.
pub(crate) enum Cmd {
    Exec(Box<dyn FnOnce(&mut Session) -> CmdResult + Send>),
    Screenshot(ShotReply),
    SetCamera {
        yaw: Option<f32>,
        pitch: Option<f32>,
        dist: Option<f32>,
        play: Option<crate::preview::PlayRequest>,
        ui: Option<crate::server::UiArg>,
        reply: ShotReply,
    },
    AgentShot {
        view: AgentView,
        width: u32,
        height: u32,
        focus: Option<Vec<String>>,
        style: ShotStyle,
        reply: ShotReply,
    },
}

/// Shared between the MCP thread and the Bevy world (the receiver sits behind a mutex,
/// since tokio's receiver is not `Sync` and Bevy resources are).
pub(crate) struct Bridge {
    pub(crate) rx: Mutex<mpsc::Receiver<Cmd>>,
    pub(crate) server_done: AtomicBool,
}

impl Bridge {
    pub(crate) fn quit(&self) -> bool {
        self.server_done.load(Ordering::Relaxed)
    }
}

/// Wakes the winit loop from the MCP thread, so a reactive (grid) or idle
/// window runs the queued command at once instead of after its wait.
pub(crate) static WAKE: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> =
    std::sync::OnceLock::new();

/// The MCP side's command sender: every send wakes the app loop.
#[derive(Clone)]
pub(crate) struct CmdSender(pub(crate) mpsc::Sender<Cmd>);

impl CmdSender {
    #[allow(clippy::result_large_err)]
    pub(crate) async fn send(&self, cmd: Cmd) -> Result<(), mpsc::error::SendError<Cmd>> {
        let r = self.0.send(cmd).await;
        if let Some(w) = WAKE.get() {
            w();
        }
        r
    }
}

/// Startup: hand the winit proxy to [`WAKE`] (absent offscreen/headless,
/// where the loop runs on a timer anyway).
pub(crate) fn install_wake(proxy: Option<Res<bevy::winit::EventLoopProxyWrapper>>) {
    let Some(p) = proxy else {
        return;
    };
    let p = std::sync::Mutex::new((**p).clone());
    let _ = WAKE.set(Box::new(move || {
        if let Ok(p) = p.lock() {
            let _ = p.send_event(bevy::winit::WinitUserEvent::WakeUp);
        }
    }));
}
