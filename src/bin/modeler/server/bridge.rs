use crate::prelude::*;

pub(crate) type CmdResult = modeler::commands::CmdResult;
pub(crate) type ShotReply = oneshot::Sender<Result<Vec<u8>, String>>;
pub(crate) type TextReply = oneshot::Sender<Result<String, String>>;

/// A unit of work for the Bevy main loop: mutate the session, or grab a picture.
pub(crate) enum Cmd {
    Exec(Box<dyn FnOnce(&mut Session) -> CmdResult + Send>),
    Screenshot(ShotReply),
    SetCamera {
        yaw: f32,
        pitch: f32,
        dist: f32,
        reply: ShotReply,
    },
    ProbeRay {
        origin: [f32; 3],
        dir: [f32; 3],
        all_hits: bool,
        reply: TextReply,
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
