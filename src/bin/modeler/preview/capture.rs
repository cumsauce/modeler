use crate::prelude::*;

/// Screenshot requests waiting for the renderer to capture a frame.
#[derive(Resource, Default)]
pub(crate) struct PendingShots(pub(crate) Vec<Mutex<Option<ShotReply>>>);

/// Ray-cast probes waiting for a frame with the current meshes: origin, dir,
/// reply. Deferred to its own system because `MeshRayCast` borrows
/// `Assets<Mesh>` immutably.
#[derive(Resource, Default)]
pub(crate) struct PendingProbes(pub(crate) Vec<Option<([f32; 3], [f32; 3], TextReply)>>);

/// Spawns one screenshot request per pending MCP tool call; the observer encodes the
/// captured frame to PNG and answers the waiting tool.
pub(crate) fn take_screenshot(mut commands: Commands, mut pending: ResMut<PendingShots>) {
    let Some(reply) = pending.0.pop() else {
        return;
    };
    commands
        .spawn(Screenshot::primary_window())
        .observe(move |captured: On<ScreenshotCaptured>| {
            if let Ok(mut slot) = reply.lock()
                && let Some(reply) = slot.take()
            {
                let _ = reply.send(encode_png(&captured.image));
            }
        })
        .insert(Name::new("mcp screenshot"));
}

/// Answers a pending `probe_ray` with the first part the ray hits: name,
/// distance along the ray and the world hit point. The deterministic check
/// for openings and occlusion — cast through a window hole to prove it is
/// open, cast at a door fill to prove it is solid.
pub(crate) fn run_probe(
    mut probes: ResMut<PendingProbes>,
    mut ray_cast: MeshRayCast,
    parts: Query<(Entity, &Name), With<PreviewPart>>,
) {
    let Some(Some((origin, dir, reply))) = probes.0.pop() else {
        return;
    };
    let Ok(direction) = Dir3::new(Vec3::from(dir).normalize_or_zero()) else {
        let _ = reply.send(Err("ray direction is zero".into()));
        return;
    };
    let ray = Ray3d {
        origin: Vec3::from(origin),
        direction,
    };
    let filter = |e| parts.contains(e);
    let settings = MeshRayCastSettings::default().with_filter(&filter);
    match ray_cast.cast_ray(ray, &settings).first() {
        Some((entity, hit)) => {
            let name = parts
                .get(*entity)
                .map(|(_, n)| n.as_str().to_string())
                .unwrap_or_else(|_| "<unknown>".into());
            let p = hit.point;
            let _ = reply.send(Ok(format!(
                "hit `{name}` at t={:.4}, point [{:.4}, {:.4}, {:.4}]",
                hit.distance, p.x, p.y, p.z
            )));
        }
        None => {
            let _ = reply.send(Ok("no hit — the ray passes clean through".into()));
        }
    }
}

/// Encode a captured frame to PNG bytes (BGRA/RGBA in, RGBA PNG out).
pub(crate) fn encode_png(image: &Image) -> Result<Vec<u8>, String> {
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
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(&rgba)
            .map_err(|e| format!("png body: {e}"))?;
    }
    Ok(out)
}
