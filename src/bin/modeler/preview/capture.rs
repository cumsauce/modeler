use crate::prelude::*;

/// Screenshot requests waiting for the renderer to capture a frame.
#[derive(Resource, Default)]
pub(crate) struct PendingShots(pub(crate) Vec<Mutex<Option<ShotReply>>>);

/// Ray-cast probes waiting for a frame with the current meshes: origin, dir,
/// reply. Deferred to its own system because `MeshRayCast` borrows
/// `Assets<Mesh>` immutably.
#[derive(Resource, Default)]
pub(crate) struct PendingProbes(pub(crate) Vec<Option<([f32; 3], [f32; 3], bool, TextReply)>>);

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
    let Some(Some((origin, dir, all_hits, reply))) = probes.0.pop() else {
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
    // By default the cast stops at the first hit; all_hits keeps going.
    let keep_going = |_: Entity| false;
    let mut settings = MeshRayCastSettings::default().with_filter(&filter);
    if all_hits {
        settings = settings.with_early_exit_test(&keep_going);
    }
    let hits = ray_cast.cast_ray(ray, &settings);
    let name_of = |entity: &Entity| {
        parts
            .get(*entity)
            .map(|(_, n)| n.as_str().to_string())
            .unwrap_or_else(|_| "<unknown>".into())
    };
    let describe = |entity: &Entity, hit: &bevy::picking::mesh_picking::ray_cast::RayMeshHit| {
        let p = hit.point;
        format!(
            "hit `{}` at t={:.4}, point [{:.4}, {:.4}, {:.4}]",
            name_of(entity),
            hit.distance,
            p.x,
            p.y,
            p.z
        )
    };
    let text = match hits.first() {
        None => "no hit — the ray passes clean through".to_string(),
        Some((entity, hit)) if !all_hits => describe(entity, hit),
        Some(_) => {
            let lines: Vec<String> = hits
                .iter()
                .enumerate()
                .map(|(i, (e, h))| format!("{}. {}", i + 1, describe(e, h)))
                .collect();
            format!(
                "{} hit(s) along the ray, nearest first:\n{}",
                lines.len(),
                lines.join("\n")
            )
        }
    };
    let _ = reply.send(Ok(text));
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
