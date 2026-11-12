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
    env_ready: Query<(), (With<MainCamera>, With<bevy::light::EnvironmentMapLight>)>,
    mut warm: Local<(u32, u32)>,
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
    let Some(reply) = pending.0.pop() else {
        return;
    };
    // The 3D view's own image: never a pane. In the grid view, the window
    // (every document's tile, as the human sees it).
    let shot = if grid.is_some_and(|g| g.on) {
        Screenshot::primary_window()
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
