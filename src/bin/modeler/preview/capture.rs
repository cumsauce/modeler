use crate::prelude::*;

/// Screenshot requests waiting for the renderer to capture a frame.
#[derive(Resource, Default)]
pub(crate) struct PendingShots(pub(crate) Vec<Mutex<Option<ShotReply>>>);

/// Spawns one screenshot request per pending MCP tool call; the observer encodes the
/// captured frame to PNG and answers the waiting tool.
pub(crate) fn take_screenshot(
    mut commands: Commands,
    mut pending: ResMut<PendingShots>,
    view: Res<View3d>,
    grid: Option<Res<Grid>>,
    wb: Option<Res<Workbench>>,
    canvas: Option<Res<crate::preview::app::UiCanvas>>,
    env_ready: Query<(), (With<MainCamera>, With<bevy::light::EnvironmentMapLight>)>,
    mut warm: Local<(u32, u32)>,
    (built, streams): (
        Option<Res<crate::preview::app::SceneBuilt>>,
        Option<Res<crate::preview::stream::Streams>>,
    ),
    mut held: Local<u32>,
    mut quiet: Local<(Option<u64>, u32)>,
) {
    // Same hold as agent shots: the environment light must be filtered.
    warm.1 = warm.1.saturating_add(1);
    if env_ready.is_empty() {
        warm.0 = 0;
    } else {
        warm.0 = warm.0.saturating_add(1);
    }
    if warm.0 < 4 && warm.1 < 120 {
        return;
    }
    if pending.0.is_empty() {
        *held = 0;
        return;
    }
    // Deterministic shots: hold until every part's mesh and bake streamed
    // in (the window's parts, or every tile in the grid), within the
    // render timeout.
    let grid_on = grid.as_ref().is_some_and(|g| g.on);
    let mut streaming = if grid_on {
        streams.as_ref().map_or(0, |s| s.pending_grid)
    } else {
        built.as_ref().map_or(0, |b| b.pending)
    };
    // A few frames after the last part landed, so the window camera's
    // temporal AA has converged on it.
    let version = built.as_ref().and_then(|b| b.version);
    if streaming > 0 || quiet.0 != version {
        *quiet = (version, 0);
    } else {
        quiet.1 = quiet.1.saturating_add(1);
    }
    if !grid_on && quiet.1 < SETTLE_AFTER_STREAM {
        streaming = streaming.max(1);
    }
    if !shot_ready(streaming, *held) {
        *held += 1;
        return;
    }
    *held = 0;
    let Some(reply) = pending.0.pop() else {
        return;
    };
    // The 3D view's own image: never a pane. In the grid view, the window
    // (every document's tile, as the human sees it).
    let shot = if grid_on || wb.is_some_and(|w| w.capture_window) {
        match canvas.and_then(|c| c.0.clone()) {
            Some(img) => Screenshot::image(img),
            None => Screenshot::primary_window(),
        }
    } else {
        Screenshot::image(view.image.clone())
    };
    commands
        .spawn(shot)
        .observe(move |captured: On<ScreenshotCaptured>| {
            if let Ok(mut slot) = reply.lock()
                && let Some(reply) = slot.take()
            {
                let _ = reply.send(encode_png(&captured.image));
            }
        })
        .insert(Name::new("mcp screenshot"));
}

/// Frames a window screenshot waits after its parts finished streaming in.
const SETTLE_AFTER_STREAM: u32 = 12;

/// Whether a screenshot may be taken: nothing is streaming in, or it has
/// waited about 12 s (60 fps) and shoots what there is rather than time out.
pub(crate) fn shot_ready(streaming: usize, held_frames: u32) -> bool {
    streaming == 0 || held_frames >= 720
}

/// Encode a captured frame to PNG bytes (BGRA/RGBA in, RGBA PNG out).
pub(crate) fn encode_png(image: &Image) -> Result<Vec<u8>, String> {
    image_to_canvas(image)?.to_png()
}

/// A captured frame as an RGBA canvas (BGRA swizzled).
pub(crate) fn image_to_canvas(image: &Image) -> Result<Canvas, String> {
    let (w, h) = (image.width(), image.height());
    let Some(data) = image.data.as_deref() else {
        return Err(format!("screenshot {w}x{h} has no pixel data"));
    };
    if w == 0 || h == 0 || data.len() != (w as usize) * (h as usize) * 4 {
        return Err(format!("unexpected screenshot {w}x{h}"));
    }
    let mut rgba = data.to_vec();
    match image.texture_descriptor.format {
        TextureFormat::Bgra8UnormSrgb | TextureFormat::Bgra8Unorm => {
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        TextureFormat::Rgba8UnormSrgb | TextureFormat::Rgba8Unorm => {}
        other => return Err(format!("unsupported screenshot format {other:?}")),
    }
    Ok(Canvas::from_rgba(w as usize, h as usize, rgba))
}

/// Tiles same-sized PNG shots row-major into one PNG, each cell labelled
/// "N: label" in its top-left corner, with 2 px separators.
pub(crate) fn compose_grid(shots: &[(Vec<u8>, String)]) -> Result<Vec<u8>, String> {
    let imgs: Vec<Canvas> = shots
        .iter()
        .map(|(png, _)| decode_png(png))
        .collect::<Result<_, _>>()?;
    let Some(first) = imgs.first() else {
        return Err("no shots to tile".into());
    };
    let (w, h) = (first.w, first.h);
    let n = imgs.len();
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let gap = 2;
    let (gw, gh) = (cols * w + (cols - 1) * gap, rows * h + (rows - 1) * gap);
    let mut out = Canvas::new(gw, gh, [90, 90, 96, 255]);
    let scale = (h / 240).max(1) * 2;
    for (k, (img, (_, label))) in imgs.iter().zip(shots).enumerate() {
        if img.w != w || img.h != h {
            return Err("grid shots differ in size".into());
        }
        let (cx, cy) = ((k % cols) * (w + gap), (k / cols) * (h + gap));
        for y in 0..h {
            let src = &img.px[y * w * 4..(y + 1) * w * 4];
            let dst = ((cy + y) * gw + cx) * 4;
            out.px[dst..dst + w * 4].copy_from_slice(src);
        }
        out.text(cx + 4, cy + 4, &format!("{}: {label}", k + 1), scale);
    }
    out.to_png()
}

#[cfg(test)]
mod tests {
    use super::shot_ready;

    #[test]
    fn screenshots_wait_for_streamed_parts() {
        // Parts still streaming in: held, until the cap.
        assert!(!shot_ready(3, 0));
        assert!(!shot_ready(1, 719));
        assert!(shot_ready(1, 720));
        // Everything in: at once.
        assert!(shot_ready(0, 0));
    }
}
