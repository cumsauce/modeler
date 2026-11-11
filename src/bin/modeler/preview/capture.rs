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

/// 5x7 bitmap glyphs (rows top to bottom, 5 low bits) for grid labels.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 17, 25, 21, 19, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [31, 2, 4, 2, 1, 17, 14],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 30, 1, 1, 17, 14],
        '6' => [6, 8, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 2, 12],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        _ => [0; 7],
    }
}

fn draw_text(buf: &mut [u8], stride: usize, x0: usize, y0: usize, text: &str, scale: usize) {
    let h = buf.len() / (stride * 4);
    // Dark backing box so the label reads on any background.
    let (bw, bh) = ((text.chars().count() * 6 + 2) * scale, 9 * scale);
    for y in y0..(y0 + bh).min(h) {
        for x in x0..(x0 + bw).min(stride) {
            let i = (y * stride + x) * 4;
            buf[i..i + 3].iter_mut().for_each(|c| *c /= 4);
            buf[i + 3] = 255;
        }
    }
    for (k, ch) in text.chars().enumerate() {
        let g = glyph(ch);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..5 {
                if bits & (16 >> col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = x0 + (1 + k * 6 + col) * scale + dx;
                        let y = y0 + (1 + row) * scale + dy;
                        if x < stride && y < h {
                            let i = (y * stride + x) * 4;
                            buf[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                        }
                    }
                }
            }
        }
    }
}

fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png decode: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("png: size overflow")?];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png decode: {e}"))?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err("png decode: expected 8-bit RGBA".into());
    }
    buf.truncate(info.buffer_size());
    Ok((info.width, info.height, buf))
}

/// Tiles same-sized PNG shots row-major into one PNG, each cell labelled
/// "N: label" in its top-left corner, with 2 px separators.
pub(crate) fn compose_grid(shots: &[(Vec<u8>, String)]) -> Result<Vec<u8>, String> {
    let imgs: Vec<(u32, u32, Vec<u8>)> = shots
        .iter()
        .map(|(png, _)| decode_png(png))
        .collect::<Result<_, _>>()?;
    let Some((w, h, _)) = imgs.first() else {
        return Err("no shots to tile".into());
    };
    let (w, h) = (*w as usize, *h as usize);
    let n = imgs.len();
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let gap = 2;
    let (gw, gh) = (cols * w + (cols - 1) * gap, rows * h + (rows - 1) * gap);
    let mut out = vec![0u8; gw * gh * 4];
    for px in out.as_chunks_mut::<4>().0 {
        *px = [90, 90, 96, 255];
    }
    let scale = (h / 240).max(1) * 2;
    for (k, ((iw, ih, data), (_, label))) in imgs.iter().zip(shots).enumerate() {
        if *iw as usize != w || *ih as usize != h {
            return Err("grid shots differ in size".into());
        }
        let (cx, cy) = ((k % cols) * (w + gap), (k / cols) * (h + gap));
        for y in 0..h {
            let src = &data[y * w * 4..(y + 1) * w * 4];
            let dst = ((cy + y) * gw + cx) * 4;
            out[dst..dst + w * 4].copy_from_slice(src);
        }
        draw_text(
            &mut out,
            gw,
            cx + 4,
            cy + 4,
            &format!("{}: {label}", k + 1),
            scale,
        );
    }
    let mut png_out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_out, gw as u32, gh as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(&out)
            .map_err(|e| format!("png body: {e}"))?;
    }
    Ok(png_out)
}
