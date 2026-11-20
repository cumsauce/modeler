//! BVH (Biovision hierarchy) reading and writing.
//!
//! HIERARCHY: `ROOT` / `JOINT` blocks with `OFFSET x y z`, `CHANNELS n ...`
//! (3 or 6 per joint, in any order) and `End Site { OFFSET }`; MOTION:
//! `Frames: N`, `Frame Time: s`, then N rows of channel values. Rotation
//! channels compose in the order written, as intrinsic rotations
//! (`Zrotation Xrotation Yrotation` = Rz * Rx * Ry, column vectors), and a
//! joint's translation is its OFFSET plus its position channels. Tabs, CRLF,
//! scientific notation and trailing spaces are fine; errors name the line.

use bevy::math::{EulerRot, Quat, Vec3};

/// One channel of a joint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chan {
    Xp,
    Yp,
    Zp,
    Xr,
    Yr,
    Zr,
}

impl Chan {
    fn parse(s: &str) -> Option<Chan> {
        Some(match s.to_ascii_lowercase().as_str() {
            "xposition" => Chan::Xp,
            "yposition" => Chan::Yp,
            "zposition" => Chan::Zp,
            "xrotation" => Chan::Xr,
            "yrotation" => Chan::Yr,
            "zrotation" => Chan::Zr,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Chan::Xp => "Xposition",
            Chan::Yp => "Yposition",
            Chan::Zp => "Zposition",
            Chan::Xr => "Xrotation",
            Chan::Yr => "Yrotation",
            Chan::Zr => "Zrotation",
        }
    }

    fn is_rot(self) -> bool {
        matches!(self, Chan::Xr | Chan::Yr | Chan::Zr)
    }
}

/// A joint: parents come before children.
#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    pub name: String,
    pub parent: Option<usize>,
    pub offset: Vec3,
    pub channels: Vec<Chan>,
    /// The `End Site` offset, when the joint has one.
    pub end: Option<Vec3>,
    /// Index of the joint's first channel in a frame row.
    pub first: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bvh {
    pub joints: Vec<Joint>,
    pub frame_time: f32,
    /// One row of channel values per frame.
    pub frames: Vec<Vec<f32>>,
    /// Things the parser let pass (a frame count that disagrees).
    pub warnings: Vec<String>,
}

struct Toks<'a> {
    t: Vec<(&'a str, usize)>,
    k: usize,
}

impl<'a> Toks<'a> {
    fn peek(&self) -> Option<(&'a str, usize)> {
        self.t.get(self.k).copied()
    }

    fn line(&self) -> usize {
        self.peek()
            .map(|t| t.1)
            .or_else(|| self.t.last().map(|t| t.1))
            .unwrap_or(1)
    }

    fn next(&mut self, what: &str) -> Result<(&'a str, usize), String> {
        let t = self
            .peek()
            .ok_or_else(|| format!("line {}: the file ends where {what} should be", self.line()))?;
        self.k += 1;
        Ok(t)
    }

    fn expect(&mut self, word: &str) -> Result<usize, String> {
        let (t, l) = self.next(&format!("`{word}`"))?;
        if !t.eq_ignore_ascii_case(word) {
            return Err(format!("line {l}: expected `{word}`, found `{t}`"));
        }
        Ok(l)
    }

    fn num(&mut self, what: &str) -> Result<f32, String> {
        let (t, l) = self.next(what)?;
        let v: f32 = t
            .parse()
            .map_err(|_| format!("line {l}: {what}: `{t}` is not a number"))?;
        if !v.is_finite() {
            return Err(format!("line {l}: {what}: `{t}` is not finite"));
        }
        Ok(v)
    }

    fn vec3(&mut self, what: &str) -> Result<Vec3, String> {
        Ok(Vec3::new(self.num(what)?, self.num(what)?, self.num(what)?))
    }

    /// The rest of `line` up to a `{` (a joint name may contain spaces).
    fn name_on(&mut self, line: usize) -> String {
        let mut parts = Vec::new();
        while let Some((t, l)) = self.peek() {
            if l != line || t == "{" {
                break;
            }
            match t.strip_suffix('{') {
                Some(s) => {
                    // `Hips{`: keep the brace for `expect`.
                    parts.push(s.to_string());
                    self.t[self.k] = ("{", l);
                    break;
                }
                None => parts.push(t.to_string()),
            }
            self.k += 1;
        }
        parts.join(" ")
    }
}

/// Parses a BVH file.
pub fn parse(text: &str) -> Result<Bvh, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let t: Vec<(&str, usize)> = text
        .lines()
        .enumerate()
        .flat_map(|(i, l)| l.split_whitespace().map(move |w| (w, i + 1)))
        .collect();
    let mut tk = Toks { t, k: 0 };
    tk.expect("HIERARCHY")?;
    let mut joints: Vec<Joint> = Vec::new();
    let mut nch = 0usize;
    let (w, l) = tk.next("ROOT")?;
    if !w.eq_ignore_ascii_case("ROOT") {
        return Err(format!("line {l}: expected `ROOT`, found `{w}`"));
    }
    block(&mut tk, &mut joints, &mut nch, None, l)?;
    if let Some((w, l)) = tk.peek()
        && w.eq_ignore_ascii_case("ROOT")
    {
        return Err(format!(
            "line {l}: a second ROOT: only one skeleton per file is supported"
        ));
    }
    tk.expect("MOTION")?;
    let (w, l) = tk.next("`Frames:`")?;
    let declared = if w.eq_ignore_ascii_case("frames:") {
        tk.num("Frames")?
    } else if let Some(n) = w.to_ascii_lowercase().strip_prefix("frames:") {
        n.parse::<f32>()
            .map_err(|_| format!("line {l}: Frames: `{n}` is not a number"))?
    } else {
        return Err(format!("line {l}: expected `Frames:`, found `{w}`"));
    };
    let (w, l) = tk.next("`Frame Time:`")?;
    if !w.eq_ignore_ascii_case("frame") {
        return Err(format!("line {l}: expected `Frame Time:`, found `{w}`"));
    }
    let (w, l) = tk.next("`Time:`")?;
    let frame_time = if w.eq_ignore_ascii_case("time:") {
        tk.num("Frame Time")?
    } else if let Some(n) = w.to_ascii_lowercase().strip_prefix("time:") {
        n.parse::<f32>()
            .map_err(|_| format!("line {l}: Frame Time: `{n}` is not a number"))?
    } else {
        return Err(format!("line {l}: expected `Frame Time:`, found `{w}`"));
    };
    if !(frame_time > 0.0 && frame_time.is_finite()) {
        return Err(format!(
            "line {l}: Frame Time must be a positive number of seconds, got {frame_time}"
        ));
    }
    if nch == 0 {
        return Err("the hierarchy has no CHANNELS".into());
    }
    let mut frames = Vec::new();
    let mut row = Vec::with_capacity(nch);
    let mut row_line = tk.line();
    while let Some((w, l)) = tk.peek() {
        tk.k += 1;
        if row.is_empty() {
            row_line = l;
        }
        let v: f32 = w.parse().map_err(|_| {
            format!(
                "line {l}: frame {}: `{w}` is not a number",
                frames.len() + 1
            )
        })?;
        if !v.is_finite() {
            return Err(format!(
                "line {l}: frame {}: `{w}` is not finite",
                frames.len() + 1
            ));
        }
        row.push(v);
        if row.len() == nch {
            frames.push(std::mem::replace(&mut row, Vec::with_capacity(nch)));
        }
    }
    if !row.is_empty() {
        return Err(format!(
            "line {row_line}: frame {} has {} of {nch} values (the hierarchy has {nch} channels)",
            frames.len() + 1,
            row.len()
        ));
    }
    if frames.is_empty() {
        return Err("MOTION has no frames".into());
    }
    let mut warnings = Vec::new();
    if declared.round() as usize != frames.len() {
        warnings.push(format!(
            "Frames: says {}, the file has {} rows (using {})",
            declared,
            frames.len(),
            frames.len()
        ));
    }
    Ok(Bvh {
        joints,
        frame_time,
        frames,
        warnings,
    })
}

fn block(
    tk: &mut Toks,
    joints: &mut Vec<Joint>,
    nch: &mut usize,
    parent: Option<usize>,
    line: usize,
) -> Result<(), String> {
    let name = tk.name_on(line);
    if name.is_empty() {
        return Err(format!("line {line}: a joint needs a name"));
    }
    tk.expect("{")?;
    let me = joints.len();
    joints.push(Joint {
        name,
        parent,
        offset: Vec3::ZERO,
        channels: Vec::new(),
        end: None,
        first: *nch,
    });
    loop {
        let (w, l) = tk.next("`}`")?;
        match w.to_ascii_uppercase().as_str() {
            "OFFSET" => joints[me].offset = tk.vec3("OFFSET")?,
            "CHANNELS" => {
                let n = tk.num("CHANNELS count")?;
                if n != 3.0 && n != 6.0 {
                    return Err(format!(
                        "line {l}: CHANNELS {n}: a joint has 3 (rotation) or 6 (position + rotation) channels"
                    ));
                }
                let mut ch = Vec::new();
                for _ in 0..n as usize {
                    let (c, cl) = tk.next("a channel name")?;
                    let c = Chan::parse(c).ok_or_else(|| {
                        format!(
                            "line {cl}: `{c}` is not a channel (Xposition, Yposition, Zposition, Xrotation, Yrotation, Zrotation)"
                        )
                    })?;
                    if ch.contains(&c) {
                        return Err(format!("line {cl}: channel {} given twice", c.label()));
                    }
                    ch.push(c);
                }
                if ch.iter().filter(|c| c.is_rot()).count() != 3 {
                    return Err(format!(
                        "line {l}: CHANNELS needs all three rotations (Xrotation, Yrotation, Zrotation)"
                    ));
                }
                joints[me].first = *nch;
                *nch += ch.len();
                joints[me].channels = ch;
            }
            "JOINT" => block(tk, joints, nch, Some(me), l)?,
            "END" => {
                tk.expect("Site")?;
                tk.expect("{")?;
                tk.expect("OFFSET")?;
                joints[me].end = Some(tk.vec3("End Site OFFSET")?);
                tk.expect("}")?;
            }
            "}" => break,
            _ => {
                return Err(format!(
                    "line {l}: unexpected `{w}` in joint `{}` (OFFSET, CHANNELS, JOINT, End Site or `}}`)",
                    joints[me].name
                ));
            }
        }
    }
    if joints[me].channels.is_empty() {
        return Err(format!(
            "line {line}: joint `{}` has no CHANNELS",
            joints[me].name
        ));
    }
    Ok(())
}

fn axis_rot(c: Chan, deg: f32) -> Quat {
    let r = deg.to_radians();
    match c {
        Chan::Xr => Quat::from_rotation_x(r),
        Chan::Yr => Quat::from_rotation_y(r),
        _ => Quat::from_rotation_z(r),
    }
}

impl Bvh {
    /// Duration from the first frame to the last (s).
    pub fn duration(&self) -> f32 {
        (self.frames.len().saturating_sub(1)) as f32 * self.frame_time
    }

    /// A joint's local translation and rotation in one frame row.
    pub fn local(&self, j: usize, row: &[f32]) -> (Vec3, Quat) {
        let jt = &self.joints[j];
        let mut t = jt.offset;
        let mut q = Quat::IDENTITY;
        for (i, c) in jt.channels.iter().enumerate() {
            let v = row.get(jt.first + i).copied().unwrap_or(0.0);
            match c {
                Chan::Xp => t.x += v,
                Chan::Yp => t.y += v,
                Chan::Zp => t.z += v,
                _ => q *= axis_rot(*c, v),
            }
        }
        (t, q.normalize())
    }

    /// Local transforms at time `t` (s), interpolated between frames
    /// (translations lerped, rotations slerped), clamped to the file.
    pub fn locals_at(&self, t: f32) -> Vec<(Vec3, Quat)> {
        let n = self.frames.len();
        let f = (t / self.frame_time).clamp(0.0, (n - 1) as f32);
        let i0 = (f.floor() as usize).min(n - 1);
        let i1 = (i0 + 1).min(n - 1);
        let u = f - i0 as f32;
        (0..self.joints.len())
            .map(|j| {
                let (t0, q0) = self.local(j, &self.frames[i0]);
                if u <= 1e-6 || i0 == i1 {
                    return (t0, q0);
                }
                let (t1, q1) = self.local(j, &self.frames[i1]);
                (t0.lerp(t1, u), q0.slerp(q1, u).normalize())
            })
            .collect()
    }

    /// World head positions and rotations from local transforms.
    pub fn world(&self, locals: &[(Vec3, Quat)]) -> Vec<(Vec3, Quat)> {
        let mut out: Vec<(Vec3, Quat)> = Vec::with_capacity(self.joints.len());
        for (j, jt) in self.joints.iter().enumerate() {
            let (t, q) = locals[j];
            out.push(match jt.parent {
                Some(p) => {
                    let (pp, pq) = out[p];
                    (pp + pq * t, (pq * q).normalize())
                }
                None => (t, q),
            });
        }
        out
    }

    /// Joint positions in the zero pose (offsets only).
    pub fn rest_positions(&self) -> Vec<Vec3> {
        let mut out: Vec<Vec3> = Vec::with_capacity(self.joints.len());
        for jt in &self.joints {
            let p = jt.parent.map_or(Vec3::ZERO, |p| out[p]);
            out.push(p + jt.offset);
        }
        out
    }

    /// The zero-pose points: every joint and every end site.
    pub fn rest_points(&self) -> Vec<Vec3> {
        let pos = self.rest_positions();
        let mut out = pos.clone();
        for (j, jt) in self.joints.iter().enumerate() {
            if let Some(e) = jt.end {
                out.push(pos[j] + e);
            }
        }
        out
    }

    /// Children of joint `j`.
    pub fn children(&self, j: usize) -> Vec<usize> {
        (0..self.joints.len())
            .filter(|&k| self.joints[k].parent == Some(j))
            .collect()
    }

    /// Whether `a` is `d` or one of its ancestors.
    pub fn is_ancestor(&self, a: usize, mut d: usize) -> bool {
        loop {
            if a == d {
                return true;
            }
            match self.joints[d].parent {
                Some(p) => d = p,
                None => return false,
            }
        }
    }
}

/// The Euler angles (degrees, in `order`'s channel order) that compose to
/// `q` as BVH reads them (`order[0]` outermost).
pub fn encode(q: Quat, order: [Chan; 3]) -> [f32; 3] {
    let rot = match order {
        [Chan::Xr, Chan::Yr, Chan::Zr] => EulerRot::XYZ,
        [Chan::Xr, Chan::Zr, Chan::Yr] => EulerRot::XZY,
        [Chan::Yr, Chan::Xr, Chan::Zr] => EulerRot::YXZ,
        [Chan::Yr, Chan::Zr, Chan::Xr] => EulerRot::YZX,
        [Chan::Zr, Chan::Yr, Chan::Xr] => EulerRot::ZYX,
        _ => EulerRot::ZXY,
    };
    let (a, b, c) = q.normalize().to_euler(rot);
    [a.to_degrees(), b.to_degrees(), c.to_degrees()]
}

/// Parses a rotation order like "ZXY".
pub fn order_of(s: &str) -> Option<[Chan; 3]> {
    let v: Vec<Chan> = s
        .chars()
        .filter_map(|c| match c.to_ascii_uppercase() {
            'X' => Some(Chan::Xr),
            'Y' => Some(Chan::Yr),
            'Z' => Some(Chan::Zr),
            _ => None,
        })
        .collect();
    match v.as_slice() {
        [a, b, c] if a != b && b != c && a != c && s.len() == 3 => Some([*a, *b, *c]),
        _ => None,
    }
}

fn f(v: f32) -> String {
    let v = if v.abs() < 1e-7 { 0.0 } else { v };
    format!("{v:.6}")
}

/// Writes a BVH file (the hierarchy depth first from the root; rows in
/// that order).
pub fn write(b: &Bvh) -> String {
    let mut s = String::from("HIERARCHY\n");
    fn rec(b: &Bvh, j: usize, depth: usize, s: &mut String) {
        let ind = "\t".repeat(depth);
        let jt = &b.joints[j];
        let kw = if jt.parent.is_none() { "ROOT" } else { "JOINT" };
        s.push_str(&format!("{ind}{kw} {}\n{ind}{{\n", jt.name));
        s.push_str(&format!(
            "{ind}\tOFFSET {} {} {}\n",
            f(jt.offset.x),
            f(jt.offset.y),
            f(jt.offset.z)
        ));
        let ch: Vec<&str> = jt.channels.iter().map(|c| c.label()).collect();
        s.push_str(&format!("{ind}\tCHANNELS {} {}\n", ch.len(), ch.join(" ")));
        for k in b.children(j) {
            rec(b, k, depth + 1, s);
        }
        if let Some(e) = jt.end {
            s.push_str(&format!(
                "{ind}\tEnd Site\n{ind}\t{{\n{ind}\t\tOFFSET {} {} {}\n{ind}\t}}\n",
                f(e.x),
                f(e.y),
                f(e.z)
            ));
        }
        s.push_str(&format!("{ind}}}\n"));
    }
    // Rows follow the order joints are written in (depth first).
    let mut dfs = Vec::new();
    fn order(b: &Bvh, j: usize, out: &mut Vec<usize>) {
        out.push(j);
        for k in b.children(j) {
            order(b, k, out);
        }
    }
    if let Some(r) = b.joints.iter().position(|j| j.parent.is_none()) {
        rec(b, r, 0, &mut s);
        order(b, r, &mut dfs);
    }
    s.push_str(&format!(
        "MOTION\nFrames: {}\nFrame Time: {}\n",
        b.frames.len(),
        f(b.frame_time)
    ));
    for row in &b.frames {
        let r: Vec<String> = dfs
            .iter()
            .flat_map(|&j| {
                let jt = &b.joints[j];
                (jt.first..jt.first + jt.channels.len())
                    .map(|i| f(row.get(i).copied().unwrap_or(0.0)))
            })
            .collect();
        s.push_str(&r.join(" "));
        s.push('\n');
    }
    s
}

/// A BVH builder: joints with channels in `order`, frames from per-joint
/// local rotations (BVH frames are world-aligned at rest) and the root's
/// position.
pub struct Builder {
    pub bvh: Bvh,
    order: [Chan; 3],
}

impl Builder {
    pub fn new(frame_time: f32, order: [Chan; 3]) -> Builder {
        Builder {
            bvh: Bvh {
                joints: Vec::new(),
                frame_time,
                frames: Vec::new(),
                warnings: Vec::new(),
            },
            order,
        }
    }

    /// Adds a joint (the first one is the root, with 6 channels).
    pub fn joint(&mut self, name: &str, parent: Option<usize>, offset: Vec3) -> usize {
        let first: usize = self.bvh.joints.iter().map(|j| j.channels.len()).sum();
        let mut channels = Vec::new();
        if parent.is_none() {
            channels.extend([Chan::Xp, Chan::Yp, Chan::Zp]);
        }
        channels.extend(self.order);
        self.bvh.joints.push(Joint {
            name: name.into(),
            parent,
            offset,
            channels,
            end: None,
            first,
        });
        self.bvh.joints.len() - 1
    }

    pub fn end(&mut self, j: usize, offset: Vec3) {
        self.bvh.joints[j].end = Some(offset);
    }

    /// Adds a frame: the root's position (added to its offset) and each
    /// joint's local rotation.
    pub fn frame(&mut self, root: Vec3, rots: &[Quat]) {
        let mut row = Vec::new();
        for (j, jt) in self.bvh.joints.iter().enumerate() {
            if jt.parent.is_none() {
                row.extend([root.x, root.y, root.z]);
            }
            let q = rots.get(j).copied().unwrap_or(Quat::IDENTITY);
            row.extend(encode(q, self.order));
        }
        self.bvh.frames.push(row);
    }
}

/// A modeler clip as BVH: one joint per bone (offsets from the rest
/// heads, end sites at leaf tails), the root's world position, and each
/// bone's rotation relative to its rest frame, sampled at `fps`.
pub fn from_clip(
    clip: &crate::anim::Clip,
    skel: &crate::anim::Skeleton,
    fps: f32,
    order: [Chan; 3],
) -> String {
    let fps = if fps > 0.0 { fps } else { 30.0 };
    let mut b = Builder::new(1.0 / fps, order);
    for (i, bone) in skel.bones.iter().enumerate() {
        let off = match bone.parent {
            Some(p) => bone.head - skel.bones[p].head,
            None => Vec3::ZERO,
        };
        let j = b.joint(&bone.name, bone.parent, off);
        if !skel.bones.iter().any(|c| c.parent == Some(i)) {
            b.end(j, bone.tail - bone.head);
        }
    }
    let n = (clip.duration * fps).round().max(1.0) as usize;
    for k in 0..=n {
        let t = (k as f32 / fps).min(clip.duration);
        let pose = clip.sample(skel, t);
        let world = skel.world(&pose);
        let g: Vec<Quat> = world
            .iter()
            .zip(&skel.bones)
            .map(|(m, bone)| {
                (m.to_scale_rotation_translation().1 * bone.rest.inverse()).normalize()
            })
            .collect();
        let rots: Vec<Quat> = skel
            .bones
            .iter()
            .enumerate()
            .map(|(i, bone)| match bone.parent {
                Some(p) => (g[p].inverse() * g[i]).normalize(),
                None => g[i],
            })
            .collect();
        let root = skel
            .bones
            .iter()
            .position(|x| x.parent.is_none())
            .map_or(Vec3::ZERO, |r| world[r].w_axis.truncate());
        b.frame(root, &rots);
    }
    write(&b.bvh)
}
