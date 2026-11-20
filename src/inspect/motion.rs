//! Motion senses over a clip: the path a bone's tip travels (`agent_views`
//! `style: "trails"`) and its height / speed / angle / position over time
//! (`inspect curves`), with the stretches where the tip is planted on the
//! ground. Everything here is pure CPU work on [`Clip::sample`], so it runs
//! headless; the 3D drawing of trails lives in the preview.

use crate::anim::{Clip, Skeleton};
use crate::raster::{Canvas, id_color_u8};
use crate::{Model, ModelerError, Vec3};
use bevy::math::Quat;
use serde_json::json;

/// A tip within this many metres of the ground (y = 0) is planted.
pub const CONTACT_Y: f32 = 0.03;
/// Dots along a trail per clip duration.
pub const TRAIL_DOTS: usize = 24;
/// Polyline segments between two dots.
const TRAIL_SUB: usize = 4;
/// Bones drawn when a trails shot picks them itself.
pub const MAX_TRAIL_BONES: usize = 8;
/// Points per line in the structured result.
const MAX_JSON_POINTS: usize = 60;

/// What `inspect curves` plots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Height,
    Speed,
    Angle,
    X,
    Z,
}

impl Channel {
    pub const ALL: [Channel; 5] = [
        Channel::Height,
        Channel::Speed,
        Channel::Angle,
        Channel::X,
        Channel::Z,
    ];

    pub fn parse(s: &str) -> Result<Channel, String> {
        Ok(match s {
            "height" => Channel::Height,
            "speed" => Channel::Speed,
            "angle" => Channel::Angle,
            "x" => Channel::X,
            "z" => Channel::Z,
            other => {
                return Err(format!(
                    "channel `{other}`: use height, speed, angle, x or z"
                ));
            }
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Channel::Height => "height",
            Channel::Speed => "speed",
            Channel::Angle => "angle",
            Channel::X => "x",
            Channel::Z => "z",
        }
    }

    /// The axis title with its unit.
    pub fn title(self) -> &'static str {
        match self {
            Channel::Height => "HEIGHT OF TIP (M)",
            Channel::Speed => "SPEED OF TIP (M/S)",
            Channel::Angle => "ANGLE FROM REST (DEG)",
            Channel::X => "TIP X (M)",
            Channel::Z => "TIP Z (M)",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Channel::Height | Channel::X | Channel::Z => "m",
            Channel::Speed => "m/s",
            Channel::Angle => "deg",
        }
    }
}

/// Per-bone series sampled at even times over the clip.
#[derive(Clone, Debug)]
pub struct BoneCurves {
    pub bone: String,
    /// The tip is the head (a root bone) rather than the tail.
    pub root: bool,
    pub x: Vec<f32>,
    pub height: Vec<f32>,
    pub z: Vec<f32>,
    pub speed: Vec<f32>,
    pub angle: Vec<f32>,
    pub contacts: Vec<(f32, f32)>,
}

impl BoneCurves {
    pub fn channel(&self, c: Channel) -> &[f32] {
        match c {
            Channel::Height => &self.height,
            Channel::Speed => &self.speed,
            Channel::Angle => &self.angle,
            Channel::X => &self.x,
            Channel::Z => &self.z,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Curves {
    pub clip: String,
    pub duration: f32,
    pub looping: bool,
    pub times: Vec<f32>,
    pub bones: Vec<BoneCurves>,
    pub key_times: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub min: f32,
    pub max: f32,
    pub t_min: f32,
    pub t_max: f32,
    pub mean: f32,
}

pub fn stats(times: &[f32], v: &[f32]) -> Stats {
    let mut s = Stats {
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
        t_min: 0.0,
        t_max: 0.0,
        mean: 0.0,
    };
    for (&t, &x) in times.iter().zip(v) {
        if x < s.min {
            s.min = x;
            s.t_min = t;
        }
        if x > s.max {
            s.max = x;
            s.t_max = t;
        }
    }
    if v.is_empty() {
        return Stats {
            min: 0.0,
            max: 0.0,
            ..s
        };
    }
    s.mean = v.iter().sum::<f32>() / v.len() as f32;
    s
}

/// The stretches `[t0, t1]` where `ys` stays at or below `thr`, with the
/// crossings interpolated between samples.
pub fn contact_spans(ts: &[f32], ys: &[f32], thr: f32) -> Vec<(f32, f32)> {
    let cross = |a: usize, b: usize| {
        let d = ys[b] - ys[a];
        if d.abs() < 1e-12 {
            ts[a]
        } else {
            ts[a] + (thr - ys[a]) / d * (ts[b] - ts[a])
        }
    };
    let mut out = Vec::new();
    let mut start: Option<f32> = None;
    for i in 0..ys.len().min(ts.len()) {
        let below = ys[i] <= thr;
        match (below, start) {
            (true, None) => start = Some(if i > 0 { cross(i - 1, i) } else { ts[i] }),
            (false, Some(s)) => {
                out.push((s, cross(i - 1, i)));
                start = None;
            }
            _ => {}
        }
    }
    if let (Some(s), Some(&last)) = (start, ts.last()) {
        out.push((s, last));
    }
    out
}

/// Per segment of a polyline of heights: both ends planted.
pub fn segment_contacts(ys: &[f32], thr: f32) -> Vec<bool> {
    ys.windows(2).map(|w| w[0] <= thr && w[1] <= thr).collect()
}

fn is_root(skel: &Skeleton, i: usize) -> bool {
    skel.bones[i].parent.is_none()
}

/// The tracked point of bone `i` in `segs`: its tail, or its head for a root.
fn tip_of(skel: &Skeleton, segs: &[(Vec3, Vec3)], i: usize) -> Vec3 {
    if is_root(skel, i) {
        segs[i].0
    } else {
        segs[i].1
    }
}

/// Tips of `bones` at each of `times`: `[time][bone]`.
fn sample_tips(clip: &Clip, skel: &Skeleton, bones: &[usize], times: &[f32]) -> Vec<Vec<Vec3>> {
    // A travelling loop plays on from where it ended: past the seam (and
    // before 0) the tips carry whole loops of root motion, so speeds don't
    // jump at the seam.
    let (delta, d) = if clip.root_motion.is_some() && clip.looping && clip.duration > 0.0 {
        (clip.root_delta(skel).0, clip.duration)
    } else {
        (Vec3::ZERO, 0.0)
    };
    times
        .iter()
        .map(|&t| {
            let loops = if d > 0.0 { t.div_euclid(d) } else { 0.0 };
            let segs = skel.posed_segments(&clip.sample(skel, t));
            bones
                .iter()
                .map(|&b| tip_of(skel, &segs, b) + delta * loops)
                .collect()
        })
        .collect()
}

/// A bone's world rotation turned from its rest frame, in degrees.
fn angle_from_rest(skel: &Skeleton, world: &[bevy::math::Mat4], i: usize) -> f32 {
    let (_, r, _) = world[i].to_scale_rotation_translation();
    let r = if skel.bones[i].rest == Quat::IDENTITY {
        r
    } else {
        (r * skel.bones[i].rest.inverse()).normalize()
    };
    r.to_axis_angle().1.to_degrees()
}

/// Which bones a trails shot or curves call covers. `globs` select by name
/// (`*` patterns); without them: the end bones that move (hands, feet, tail
/// tips) and the head, plus the root, the most travelled first, at most
/// [`MAX_TRAIL_BONES`].
pub fn pick_bones(
    clip: &Clip,
    skel: &Skeleton,
    globs: Option<&[String]>,
) -> Result<Vec<usize>, String> {
    if let Some(globs) = globs {
        let out: Vec<usize> = (0..skel.bones.len())
            .filter(|&i| {
                globs
                    .iter()
                    .any(|g| crate::name_matches(g, &skel.bones[i].name))
            })
            .collect();
        if out.is_empty() {
            let names: Vec<&str> = skel.bones.iter().map(|b| b.name.as_str()).collect();
            return Err(format!(
                "bones {globs:?} match no bone of `{}` (bones: {})",
                skel.name,
                names.join(", ")
            ));
        }
        return Ok(out);
    }
    let n = skel.bones.len();
    let mut has_child = vec![false; n];
    for b in &skel.bones {
        if let Some(p) = b.parent {
            has_child[p] = true;
        }
    }
    let cands: Vec<usize> = (0..n)
        .filter(|&i| is_root(skel, i) || !has_child[i] || skel.bones[i].name.starts_with("head"))
        .collect();
    let steps = 48;
    let times: Vec<f32> = (0..=steps)
        .map(|k| clip.duration * k as f32 / steps as f32)
        .collect();
    let tips = sample_tips(clip, skel, &cands, &times);
    let mut ranked: Vec<(usize, f32, bool)> = cands
        .iter()
        .enumerate()
        .map(|(c, &b)| {
            let len: f32 = tips.windows(2).map(|w| (w[1][c] - w[0][c]).length()).sum();
            (b, len, is_root(skel, b))
        })
        .filter(|&(_, len, root)| root || len > 0.005)
        .collect();
    ranked.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.total_cmp(&a.1)));
    let mut out: Vec<usize> = ranked
        .into_iter()
        .take(MAX_TRAIL_BONES)
        .map(|r| r.0)
        .collect();
    out.sort_unstable();
    if out.is_empty() {
        return Err("the skeleton has no bones".into());
    }
    Ok(out)
}

/// Sample the chosen bones at `samples` even steps over the clip (the end
/// included: `samples + 1` points).
pub fn curves(clip: &Clip, skel: &Skeleton, bones: &[usize], samples: usize) -> Curves {
    let n = samples.max(2);
    let dur = clip.duration.max(1e-6);
    let dt = dur / n as f32;
    // One extra step either side, for central differences at the ends.
    let times: Vec<f32> = (-1..=(n as i32 + 1)).map(|k| k as f32 * dt).collect();
    let tips = sample_tips(clip, skel, bones, &times);
    let worlds: Vec<Vec<bevy::math::Mat4>> = times[1..=n + 1]
        .iter()
        .map(|&t| skel.world(&clip.sample(skel, t)))
        .collect();
    let ts: Vec<f32> = times[1..=n + 1].to_vec();
    let out = bones
        .iter()
        .enumerate()
        .map(|(c, &b)| {
            let p = |k: usize| tips[k][c]; // index into `times`: k = step + 1
            let mut speed = Vec::with_capacity(n + 1);
            for s in 0..=n {
                let k = s + 1;
                let v = if !clip.looping && s == 0 {
                    (p(k + 1) - p(k)).length() / dt
                } else if !clip.looping && s == n {
                    (p(k) - p(k - 1)).length() / dt
                } else {
                    (p(k + 1) - p(k - 1)).length() / (2.0 * dt)
                };
                speed.push(v);
            }
            let pts: Vec<Vec3> = (1..=n + 1).map(p).collect();
            let height: Vec<f32> = pts.iter().map(|v| v.y).collect();
            BoneCurves {
                bone: skel.bones[b].name.clone(),
                root: is_root(skel, b),
                x: pts.iter().map(|v| v.x).collect(),
                z: pts.iter().map(|v| v.z).collect(),
                speed,
                angle: worlds.iter().map(|w| angle_from_rest(skel, w, b)).collect(),
                contacts: contact_spans(&ts, &height, CONTACT_Y),
                height,
            }
        })
        .collect();
    let mut key_times: Vec<f32> = clip
        .key_times()
        .into_iter()
        .map(|t| t.clamp(0.0, dur))
        .collect();
    key_times.sort_by(f32::total_cmp);
    key_times.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
    Curves {
        clip: clip.name.clone(),
        duration: clip.duration,
        looping: clip.looping,
        times: ts,
        bones: out,
        key_times,
    }
}

/// The path of one bone's tip over the whole clip.
#[derive(Clone, Debug)]
pub struct Trail {
    pub bone: String,
    pub root: bool,
    /// The polyline, `TRAIL_SUB` segments per dot step.
    pub pts: Vec<Vec3>,
    /// Even time steps along it (the first is t = 0); a looping clip's end
    /// is its start, so it has no dot of its own.
    pub dots: Vec<Vec3>,
    /// Per polyline segment: the tip is planted.
    pub planted: Vec<bool>,
}

pub fn trails(clip: &Clip, skel: &Skeleton, bones: &[usize], dots: usize) -> Vec<Trail> {
    let steps = dots.max(2) * TRAIL_SUB;
    let times: Vec<f32> = (0..=steps)
        .map(|k| clip.duration * k as f32 / steps as f32)
        .collect();
    let tips = sample_tips(clip, skel, bones, &times);
    bones
        .iter()
        .enumerate()
        .map(|(c, &b)| {
            let pts: Vec<Vec3> = tips.iter().map(|t| t[c]).collect();
            let ys: Vec<f32> = pts.iter().map(|p| p.y).collect();
            let ndots = if clip.looping { dots } else { dots + 1 };
            Trail {
                bone: skel.bones[b].name.clone(),
                root: is_root(skel, b),
                dots: (0..ndots).map(|k| pts[k * TRAIL_SUB]).collect(),
                planted: segment_contacts(&ys, CONTACT_Y),
                pts,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The plot image
// ---------------------------------------------------------------------------

const BG: [u8; 4] = [20, 22, 28, 255];
const PLOT_BG: [u8; 4] = [32, 35, 44, 255];
const GRID: [u8; 4] = [56, 60, 74, 255];
const KEY: [u8; 4] = [74, 80, 98, 255];
const AXIS: [u8; 4] = [150, 156, 170, 255];

/// A 1-2-5 tick step and the bounds it rounds `[lo, hi]` out to.
fn nice_range(lo: f32, hi: f32, target: usize) -> (f32, f32, f32) {
    let (lo, hi) = if hi - lo < 1e-6 {
        (lo - 0.5, hi + 0.5)
    } else {
        (lo, hi)
    };
    let raw = (hi - lo) / target.max(1) as f32;
    let mag = 10f32.powf(raw.log10().floor());
    let f = raw / mag;
    let step = mag
        * if f <= 1.0 {
            1.0
        } else if f <= 2.0 {
            2.0
        } else if f <= 5.0 {
            5.0
        } else {
            10.0
        };
    ((lo / step).floor() * step, (hi / step).ceil() * step, step)
}

fn fmt_tick(v: f32, step: f32) -> String {
    let dec = (-step.log10().floor()).clamp(0.0, 4.0) as usize;
    let s = format!("{v:.dec$}");
    if s.starts_with("-0")
        && s.trim_start_matches("-")
            .trim_matches(['0', '.'])
            .is_empty()
    {
        s.trim_start_matches('-').to_string()
    } else {
        s
    }
}

/// Small-multiple line plots: one row per channel, one line per bone, key
/// times as faint ticks, planted spans as bars underneath.
pub fn plot(c: &Curves, channels: &[Channel]) -> Canvas {
    const W: usize = 960;
    const LEFT: usize = 92;
    const RIGHT: usize = 20;
    const PLOT_H: usize = 128;
    const PITCH: usize = PLOT_H + 40;
    let scale = 2;
    // Legend rows (wrapped).
    let mut legend: Vec<(usize, usize, usize)> = Vec::new(); // (x, y, bone)
    let (mut lx, mut ly) = (LEFT, 8);
    for (i, b) in c.bones.iter().enumerate() {
        let wd = Canvas::text_width(&b.bone, scale) + 26;
        if lx + wd > W - RIGHT {
            lx = LEFT;
            ly += 24;
        }
        legend.push((lx, ly, i));
        lx += wd + 8;
    }
    let legend_h = ly + 30;
    let strip_h = c.bones.len() * 12 + 8;
    let h = legend_h + channels.len() * PITCH + strip_h + 34;
    let mut cv = Canvas::new(W, h, BG);
    for &(x, y, i) in &legend {
        let col = id_color_u8(i);
        for dy in 0..14 {
            for dx in 0..14 {
                cv.set((x + dx) as i64, (y + dy) as i64 + 2, col);
            }
        }
        let mut name = c.bones[i].bone.clone();
        if c.bones[i].root {
            name.push('*');
        }
        cv.text(x + 18, y, &name, scale);
    }
    let pw = (W - LEFT - RIGHT) as f64;
    let dur = c.duration.max(1e-6) as f64;
    let tx = |t: f32| LEFT as f64 + t as f64 / dur * pw;
    let draw_xticks = |cv: &mut Canvas, y: usize| {
        let (_, hi, step) = nice_range(0.0, c.duration, 8);
        let mut v = 0.0;
        while v <= hi + 1e-6 && v <= c.duration + 1e-4 {
            let x = tx(v).round() as usize;
            cv.text(
                x.saturating_sub(Canvas::text_width(&fmt_tick(v, step), scale) / 2),
                y,
                &fmt_tick(v, step),
                scale,
            );
            v += step;
        }
    };
    for (row, &ch) in channels.iter().enumerate() {
        let y0 = legend_h + row * PITCH;
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for b in &c.bones {
            for &v in b.channel(ch) {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if ch == Channel::Speed || ch == Channel::Angle {
            lo = lo.min(0.0);
        }
        if !lo.is_finite() || !hi.is_finite() {
            (lo, hi) = (0.0, 1.0);
        }
        let (nlo, nhi, step) = nice_range(lo, hi, 6);
        let py =
            |v: f32| (y0 + PLOT_H) as f64 - (v - nlo) as f64 / (nhi - nlo) as f64 * PLOT_H as f64;
        // Panel, grid, key times.
        for y in y0..=y0 + PLOT_H {
            for x in LEFT..W - RIGHT {
                cv.set(x as i64, y as i64, PLOT_BG);
            }
        }
        for &t in &c.key_times {
            let x = tx(t);
            cv.line((x, y0 as f64), (x, (y0 + PLOT_H) as f64), KEY, 1);
        }
        let mut v = nlo;
        while v <= nhi + step * 1e-3 {
            let y = py(v);
            cv.line((LEFT as f64, y), ((W - RIGHT) as f64, y), GRID, 1);
            let s = fmt_tick(v, step);
            cv.text(
                LEFT.saturating_sub(Canvas::text_width(&s, scale) + 4),
                (y.round() as usize).saturating_sub(9),
                &s,
                scale,
            );
            v += step;
        }
        if ch == Channel::Height && nlo < CONTACT_Y && nhi > CONTACT_Y {
            let y = py(CONTACT_Y);
            for x in (LEFT..W - RIGHT).step_by(8) {
                cv.line((x as f64, y), ((x + 4) as f64, y), AXIS, 1);
            }
        }
        cv.line(
            (LEFT as f64, y0 as f64),
            (LEFT as f64, (y0 + PLOT_H) as f64),
            AXIS,
            1,
        );
        cv.line(
            (LEFT as f64, (y0 + PLOT_H) as f64),
            ((W - RIGHT) as f64, (y0 + PLOT_H) as f64),
            AXIS,
            1,
        );
        for (i, b) in c.bones.iter().enumerate() {
            let col = id_color_u8(i);
            let ys = b.channel(ch);
            for k in 1..ys.len().min(c.times.len()) {
                cv.line(
                    (tx(c.times[k - 1]), py(ys[k - 1])),
                    (tx(c.times[k]), py(ys[k])),
                    col,
                    2,
                );
            }
        }
        cv.text(LEFT + 6, y0 + 4, ch.title(), scale);
        if row + 1 < channels.len() {
            draw_xticks(&mut cv, y0 + PLOT_H + 6);
        }
    }
    // Planted spans.
    let sy = legend_h + channels.len() * PITCH - 10;
    cv.text(LEFT + 6, sy - 14, "PLANTED: TIP BELOW 3 CM", scale);
    for (i, b) in c.bones.iter().enumerate() {
        let y = sy + 6 + i * 12;
        let col = id_color_u8(i);
        for x in LEFT..W - RIGHT {
            for dy in 0..9 {
                cv.set(x as i64, (y + dy) as i64, PLOT_BG);
            }
        }
        for &(t0, t1) in &b.contacts {
            let (x0, x1) = (
                tx(t0).round() as i64,
                tx(t1).round().max(tx(t0).round() + 2.0) as i64,
            );
            for x in x0..=x1 {
                for dy in 0..9 {
                    cv.set(x, (y + dy) as i64, col);
                }
            }
        }
        for &t in &c.key_times {
            let x = tx(t).round() as i64;
            cv.set(x, y as i64 + 9, KEY);
        }
    }
    let xl = sy + 6 + c.bones.len() * 12 + 6;
    draw_xticks(&mut cv, xl);
    cv.text(LEFT + (pw as usize) / 2 - 40, xl + 20, "TIME (S)", scale);
    cv
}

/// An f32 rounded to 4 decimals as an f64, so JSON prints `0.0167`, not
/// `0.016699999570846558`.
fn r4(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

fn downsample(times: &[f32], v: &[f32]) -> Vec<[f64; 2]> {
    let n = v.len().min(times.len());
    if n == 0 {
        return Vec::new();
    }
    let m = n.min(MAX_JSON_POINTS);
    (0..m)
        .map(|k| {
            let i = if m == 1 { 0 } else { k * (n - 1) / (m - 1) };
            [r4(times[i]), r4(v[i])]
        })
        .collect()
}

/// `inspect curves`: statistics, planted spans, the sampled series and the
/// plot (PNG) of the chosen bones' tips over a clip.
pub fn report(
    model: &Model,
    clip: Option<&str>,
    bones: Option<&[String]>,
    channels: Option<&[String]>,
    samples: Option<usize>,
) -> Result<(String, serde_json::Value, Vec<u8>), ModelerError> {
    let (c, s) = crate::anim::clip(model, clip)?;
    let chans: Vec<Channel> = match channels {
        Some(list) if !list.is_empty() => {
            let mut v = Vec::new();
            for n in list {
                let ch = Channel::parse(n)?;
                if !v.contains(&ch) {
                    v.push(ch);
                }
            }
            v
        }
        _ => vec![Channel::Height, Channel::Speed, Channel::Angle],
    };
    let n = samples.unwrap_or(120);
    if !(8..=2000).contains(&n) {
        return Err("samples: 8..2000".to_string().into());
    }
    let picked = pick_bones(&c, &s, bones)?;
    let cur = curves(&c, &s, &picked, n);
    let mut lines = vec![format!(
        "curves of clip `{}` ({} s{}), {} samples, tips of {} bone(s) (a root's tip is its head); planted = tip at or below {} cm:",
        c.name,
        c.duration,
        if c.looping { ", loop" } else { "" },
        n,
        cur.bones.len(),
        CONTACT_Y * 100.0
    )];
    let mut rows = Vec::new();
    for b in &cur.bones {
        let spans = if b.contacts.is_empty() {
            "never planted".to_string()
        } else {
            format!(
                "planted {}",
                b.contacts
                    .iter()
                    .map(|(a, z)| format!("[{a:.2}, {z:.2}]"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        lines.push(format!("- `{}`: {spans}", b.bone));
        let mut chj = serde_json::Map::new();
        for &ch in &chans {
            let v = b.channel(ch);
            let st = stats(&cur.times, v);
            lines.push(format!(
                "    {} ({}): min {:.3} at t={:.2}, max {:.3} at t={:.2}, mean {:.3}",
                ch.name(),
                ch.unit(),
                st.min,
                st.t_min,
                st.max,
                st.t_max,
                st.mean
            ));
            chj.insert(
                ch.name().into(),
                json!({
                    "unit": ch.unit(), "min": r4(st.min), "max": r4(st.max),
                    "t_min": r4(st.t_min), "t_max": r4(st.t_max), "mean": r4(st.mean),
                    "series": downsample(&cur.times, v),
                }),
            );
        }
        rows.push(json!({
            "bone": b.bone,
            "tip": if b.root { "head" } else { "tail" },
            "contacts": b.contacts.iter().map(|(a, z)| [r4(*a), r4(*z)]).collect::<Vec<_>>(),
            "channels": chj,
        }));
    }
    let png = plot(&cur, &chans).to_png()?;
    Ok((
        lines.join("\n"),
        json!({
            "clip": c.name, "duration": r4(c.duration), "loop": c.looping,
            "samples": n, "contact_height": r4(CONTACT_Y),
            "key_times": cur.key_times.iter().map(|t| r4(*t)).collect::<Vec<_>>(),
            "bones": rows,
        }),
        png,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(items: serde_json::Value) -> crate::commands::Session {
        let mut s = crate::commands::Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!("items must be an array")
        };
        s.apply_values(None, items)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    /// A root with an arm whose tail rises and falls by a sine (1 s), and a
    /// foot that is planted for the first half second then lifts.
    fn rig() -> crate::commands::Session {
        session(json!([
            {"type": "skeleton", "name": "rig", "bones": {
                "hip": {"head": [0, 0.5, 0], "tail": [0, 0.6, 0]},
                "arm": {"parent": "hip", "head": [0, 0.5, 0], "tail": [0, 1.0, 0]},
                "foot": {"parent": "hip", "head": [0.2, 0.1, 0], "tail": [0.2, 0.0, 0]}}},
            {"type": "clip", "name": "move", "skeleton": "rig", "duration": 1.0,
             "interp": "linear", "tracks": {
                "arm": {"translate": [
                    [0, [0, 0, 0]], [0.25, [0, 0.2, 0]], [0.5, [0, 0, 0]],
                    [0.75, [0, -0.2, 0]], [1, [0, 0, 0]]]},
                "foot": {"translate": [
                    [0, [0, 0, 0]], [0.5, [0, 0, 0]], [0.75, [0, 0.2, 0]], [1, [0, 0, 0]]]}}}
        ]))
    }

    fn all(s: &crate::commands::Session) -> (Clip, Skeleton) {
        crate::anim::clip(&s.model, Some("move")).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn extremes_and_means_of_a_sine_tip() {
        let s = rig();
        let (c, k) = all(&s);
        let arm = k.bone("arm").unwrap_or(0);
        let cur = curves(&c, &k, &[arm], 100);
        let b = &cur.bones[0];
        let st = stats(&cur.times, &b.height);
        assert!((st.max - 1.2).abs() < 1e-3, "{st:?}");
        assert!((st.t_max - 0.25).abs() < 0.011, "{st:?}");
        assert!((st.min - 0.8).abs() < 1e-3, "{st:?}");
        assert!((st.t_min - 0.75).abs() < 0.011, "{st:?}");
        // Speed peaks where the tip passes its rest height: 0.2 m in 0.25 s.
        let sp = stats(&cur.times, &b.speed);
        assert!((sp.max - 0.8).abs() < 0.02, "{sp:?}");
        assert!(sp.min >= 0.0);
        assert_eq!(cur.times.len(), 101);
        assert_eq!(cur.key_times.first().copied(), Some(0.0));
        assert!(b.contacts.is_empty());
    }

    #[test]
    fn a_planted_span_has_the_right_bounds() {
        let s = rig();
        let (c, k) = all(&s);
        let foot = k.bone("foot").unwrap_or(0);
        let cur = curves(&c, &k, &[foot], 200);
        let spans = &cur.bones[0].contacts;
        // On the ground until 0.5 s, then rises 0.2 m by 0.75 s: the 3 cm
        // line is crossed at 0.5 + 0.25 * 0.15.
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(spans[0].0.abs() < 1e-4, "{spans:?}");
        assert!((spans[0].1 - 0.5375).abs() < 2e-3, "{spans:?}");
        assert!((spans[1].1 - 1.0).abs() < 1e-4, "{spans:?}");
    }

    #[test]
    fn contact_spans_interpolate_the_crossings() {
        let ts = [0.0, 1.0, 2.0, 3.0, 4.0];
        let ys = [0.1, 0.0, 0.0, 0.1, 0.1];
        let s = contact_spans(&ts, &ys, 0.03);
        assert_eq!(s.len(), 1);
        assert!(
            (s[0].0 - 0.7).abs() < 1e-5 && (s[0].1 - 2.3).abs() < 1e-5,
            "{s:?}"
        );
        assert_eq!(segment_contacts(&ys, 0.03), vec![false, true, false, false]);
    }

    #[test]
    fn default_bones_are_moving_ends_and_the_root() {
        let s = rig();
        let (c, k) = all(&s);
        let names: Vec<&str> = pick_bones(&c, &k, None)
            .unwrap_or_default()
            .into_iter()
            .map(|i| k.bones[i].name.as_str())
            .collect();
        assert_eq!(names, vec!["hip", "arm", "foot"]);
        let one = pick_bones(&c, &k, Some(&["ar*".to_string()])).unwrap_or_default();
        assert_eq!(one, vec![1]);
        assert!(pick_bones(&c, &k, Some(&["zzz".to_string()])).is_err());
    }

    #[test]
    fn trails_have_dots_at_even_steps_and_flag_planted_segments() {
        let s = rig();
        let (c, k) = all(&s);
        let foot = k.bone("foot").unwrap_or(0);
        let t = trails(&c, &k, &[foot], 24);
        assert_eq!(t[0].dots.len(), 24);
        assert_eq!(t[0].pts.len(), 24 * TRAIL_SUB + 1);
        assert_eq!(t[0].planted.len(), 24 * TRAIL_SUB);
        assert!(t[0].planted[0] && !t[0].planted[60]);
        assert!(!t[0].root);
        let hip = trails(&c, &k, &[0], 24);
        assert!(hip[0].root);
        assert!((hip[0].pts[0].y - 0.5).abs() < 1e-5, "root tip is its head");
    }

    #[test]
    fn the_plot_image_has_its_size_and_draws_lines() {
        let s = rig();
        let (c, k) = all(&s);
        let cur = curves(&c, &k, &[0, 1, 2], 120);
        let cv = plot(&cur, &[Channel::Height, Channel::Speed, Channel::Angle]);
        assert_eq!(cv.w, 960);
        assert!(cv.h > 400 && cv.h < 1200, "{}", cv.h);
        assert_eq!(cv.px.len(), cv.w * cv.h * 4);
        let bg = &cv.px[0..4];
        let lit = cv.px.chunks(4).filter(|p| *p != bg).count();
        assert!(lit > 5000, "{lit}");
        assert!(cv.to_png().is_ok_and(|p| p.len() > 500));
    }

    #[test]
    fn the_report_gives_numbers_and_an_image() {
        let s = rig();
        let (t, j, png) =
            report(&s.model, None, None, None, Some(60)).unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("foot") && t.contains("planted"), "{t}");
        let series = &j["bones"][1]["channels"]["height"]["series"];
        assert!(
            series
                .as_array()
                .is_some_and(|a| a.len() <= 60 && !a.is_empty())
        );
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(report(&s.model, None, None, Some(&["wobble".to_string()]), None).is_err());
    }
}
