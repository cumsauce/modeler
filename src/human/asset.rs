//! The human template's data file (`data/human/human.bin`): MakeHuman's
//! CC0 base mesh, skeleton, weights and morph targets, packed by
//! `examples/pack_human.rs` into one zstd-compressed little-endian blob.
//!
//! Layout (after decompression), all little-endian:
//! - magic `MHUM`, u32 version
//! - str source note
//! - u32 n, n x [f32; 3] vertex positions (meters, y up, facing +z)
//! - u32 n, n x [f32; 2] uvs
//! - u16 n, n x str group names
//! - u32 n faces: u8 group, 4 x u16 vertex, 4 x u32 uv (quads)
//! - u32 n joint refs: str name, u16 k, k x u16 vertex (a joint is the
//!   mean of its vertices, so it follows the morphs)
//! - u16 n bones: str name, i16 parent, u16 head ref, u16 tail ref
//! - per bone: u32 k, k x u16 vertex, k x u16 weight (x / 65535)
//! - u32 n targets: str name, u32 k, k x u16 vertex (delta-coded,
//!   wrapping), then k x i16 dx, k x i16 dy, k x i16 dz (units of 0.1 mm)
//! - u32 n keys: str name, i32 low target, i32 high target (-1 none)
//! - u32 n expression units: str name, 3 x i32 target (african, asian,
//!   caucasian)
//!
//! A `str` is a u16 byte length and UTF-8.

use std::sync::{Arc, OnceLock};

pub const MAGIC: &[u8; 4] = b"MHUM";
pub const VERSION: u32 = 1;
/// Target deltas are stored in units of 0.1 mm.
pub const QUANTUM: f32 = 1e-4;

#[derive(Clone, Debug, Default)]
pub struct Face {
    pub group: u8,
    pub v: [u16; 4],
    pub t: [u32; 4],
}

#[derive(Clone, Debug, Default)]
pub struct Bone {
    pub name: String,
    pub parent: i16,
    pub head: u16,
    pub tail: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Target {
    pub name: String,
    pub idx: Vec<u16>,
    pub d: Vec<[i16; 3]>,
}

/// A detail slider: -1..0 drives `low`, 0..1 drives `high` (either may be
/// absent: a one-sided 0..1 key).
#[derive(Clone, Debug, Default)]
pub struct Key {
    pub name: String,
    pub low: i32,
    pub high: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Unit {
    pub name: String,
    /// Target per ethnicity: african, asian, caucasian (-1 none).
    pub by_ethnicity: [i32; 3],
}

#[derive(Clone, Debug, Default)]
pub struct HumanAsset {
    pub source: String,
    pub verts: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub groups: Vec<String>,
    pub faces: Vec<Face>,
    pub joints: Vec<(String, Vec<u16>)>,
    pub bones: Vec<Bone>,
    /// Per bone: (vertex, weight).
    pub weights: Vec<Vec<(u16, f32)>>,
    pub targets: Vec<Target>,
    pub keys: Vec<Key>,
    pub units: Vec<Unit>,
    /// Target index by name (built on load).
    pub by_name: std::collections::HashMap<String, usize>,
}

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u16(s.len() as u16);
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct R<'a> {
    b: &'a [u8],
    at: usize,
}

impl R<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.b.len());
        let Some(end) = end else {
            return Err(format!("truncated at byte {}", self.at));
        };
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.arr::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(i16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.arr()?))
    }
    fn str(&mut self) -> Result<String, String> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|e| e.to_string())
    }
    fn len(&mut self, max: usize) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(format!("implausible count {n} at byte {}", self.at));
        }
        Ok(n)
    }
}

impl HumanAsset {
    /// The uncompressed payload (see the module docs).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.u32(VERSION);
        w.str(&self.source);
        w.u32(self.verts.len() as u32);
        for v in &self.verts {
            v.iter().for_each(|x| w.f32(*x));
        }
        w.u32(self.uvs.len() as u32);
        for v in &self.uvs {
            v.iter().for_each(|x| w.f32(*x));
        }
        w.u16(self.groups.len() as u16);
        self.groups.iter().for_each(|g| w.str(g));
        w.u32(self.faces.len() as u32);
        for f in &self.faces {
            w.u8(f.group);
            f.v.iter().for_each(|x| w.u16(*x));
            f.t.iter().for_each(|x| w.u32(*x));
        }
        w.u32(self.joints.len() as u32);
        for (n, vs) in &self.joints {
            w.str(n);
            w.u16(vs.len() as u16);
            vs.iter().for_each(|x| w.u16(*x));
        }
        w.u16(self.bones.len() as u16);
        for b in &self.bones {
            w.str(&b.name);
            w.i16(b.parent);
            w.u16(b.head);
            w.u16(b.tail);
        }
        for i in 0..self.bones.len() {
            let ws = self.weights.get(i).map_or(&[][..], |v| v.as_slice());
            w.u32(ws.len() as u32);
            ws.iter().for_each(|x| w.u16(x.0));
            ws.iter()
                .for_each(|x| w.u16((x.1.clamp(0.0, 1.0) * 65535.0).round() as u16));
        }
        w.u32(self.targets.len() as u32);
        for t in &self.targets {
            w.str(&t.name);
            w.u32(t.idx.len() as u32);
            let mut last = 0u16;
            for &i in &t.idx {
                w.u16(i.wrapping_sub(last));
                last = i;
            }
            for c in 0..3 {
                t.d.iter().for_each(|d| w.i16(d[c]));
            }
        }
        w.u32(self.keys.len() as u32);
        for k in &self.keys {
            w.str(&k.name);
            w.i32(k.low);
            w.i32(k.high);
        }
        w.u32(self.units.len() as u32);
        for u in &self.units {
            w.str(&u.name);
            u.by_ethnicity.iter().for_each(|x| w.i32(*x));
        }
        w.0
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, String> {
        let mut r = R { b, at: 0 };
        if r.take(4)? != MAGIC {
            return Err("not a human template (bad magic)".into());
        }
        let v = r.u32()?;
        if v != VERSION {
            return Err(format!(
                "human template version {v}, this build reads {VERSION}: re-pack it"
            ));
        }
        const MAX: usize = 1 << 24;
        let mut a = HumanAsset {
            source: r.str()?,
            ..HumanAsset::default()
        };
        let n = r.len(65536)?;
        for _ in 0..n {
            a.verts.push([r.f32()?, r.f32()?, r.f32()?]);
        }
        let n = r.len(MAX)?;
        for _ in 0..n {
            a.uvs.push([r.f32()?, r.f32()?]);
        }
        let n = r.u16()?;
        for _ in 0..n {
            a.groups.push(r.str()?);
        }
        let n = r.len(MAX)?;
        for _ in 0..n {
            let group = r.u8()?;
            let v = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
            let t = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
            a.faces.push(Face { group, v, t });
        }
        let n = r.len(MAX)?;
        for _ in 0..n {
            let name = r.str()?;
            let k = r.u16()?;
            let vs = (0..k).map(|_| r.u16()).collect::<Result<Vec<_>, _>>()?;
            a.joints.push((name, vs));
        }
        let n = r.u16()?;
        for _ in 0..n {
            a.bones.push(Bone {
                name: r.str()?,
                parent: r.i16()?,
                head: r.u16()?,
                tail: r.u16()?,
            });
        }
        for _ in 0..n {
            let k = r.len(MAX)?;
            let idx = (0..k).map(|_| r.u16()).collect::<Result<Vec<_>, _>>()?;
            let ws = (0..k).map(|_| r.u16()).collect::<Result<Vec<_>, _>>()?;
            a.weights.push(
                idx.into_iter()
                    .zip(ws)
                    .map(|(i, w)| (i, w as f32 / 65535.0))
                    .collect(),
            );
        }
        let n = r.len(MAX)?;
        for _ in 0..n {
            let name = r.str()?;
            let k = r.len(65536 * 4)?;
            let mut idx = Vec::with_capacity(k);
            let mut last = 0u16;
            for _ in 0..k {
                last = last.wrapping_add(r.u16()?);
                idx.push(last);
            }
            let mut d = vec![[0i16; 3]; k];
            for c in 0..3 {
                for x in d.iter_mut() {
                    x[c] = r.i16()?;
                }
            }
            a.targets.push(Target { name, idx, d });
        }
        let n = r.len(MAX)?;
        for _ in 0..n {
            a.keys.push(Key {
                name: r.str()?,
                low: r.i32()?,
                high: r.i32()?,
            });
        }
        let n = r.len(MAX)?;
        for _ in 0..n {
            a.units.push(Unit {
                name: r.str()?,
                by_ethnicity: [r.i32()?, r.i32()?, r.i32()?],
            });
        }
        let nv = a.verts.len();
        let bad = a
            .faces
            .iter()
            .any(|f| f.v.iter().any(|&i| i as usize >= nv))
            || a.targets
                .iter()
                .any(|t| t.idx.iter().any(|&i| i as usize >= nv))
            || a.joints
                .iter()
                .any(|j| j.1.iter().any(|&i| i as usize >= nv))
            || a.faces
                .iter()
                .any(|f| f.t.iter().any(|&i| i as usize >= a.uvs.len()));
        if bad {
            return Err("an index is out of range".into());
        }
        a.by_name = a
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.clone(), i))
            .collect();
        Ok(a)
    }

    /// Compressed file bytes.
    pub fn encode(&self) -> Vec<u8> {
        ruzstd::encoding::compress_to_vec(
            self.to_bytes().as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        )
    }

    pub fn decode(file: &[u8]) -> Result<Self, String> {
        use std::io::Read;
        let mut src = file;
        let mut dec =
            ruzstd::decoding::StreamingDecoder::new(&mut src).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        dec.read_to_end(&mut out).map_err(|e| e.to_string())?;
        Self::from_bytes(&out)
    }

    pub fn target(&self, name: &str) -> Option<&Target> {
        self.target_index(name).map(|i| &self.targets[i])
    }

    pub fn target_index(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }
}

pub const ENV: &str = "MODELER_HUMAN_ASSET";

/// Where the file is looked for, in order: `$MODELER_HUMAN_ASSET`, beside
/// the running binary (`human.bin` or `data/human/human.bin`), the
/// repository this was built from, and `data/human/human.bin` under the
/// working directory.
pub fn candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var(ENV) {
        out.push(p.into());
        return out;
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
    {
        out.push(dir.join("human.bin"));
        out.push(dir.join("data/human/human.bin"));
    }
    out.push(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/human/human.bin"));
    out.push("data/human/human.bin".into());
    out
}

/// The process-wide template, loaded once.
pub fn load() -> Result<Arc<HumanAsset>, String> {
    static A: OnceLock<Result<Arc<HumanAsset>, String>> = OnceLock::new();
    A.get_or_init(|| {
        let tried = candidates();
        for p in &tried {
            if let Ok(b) = std::fs::read(p) {
                return HumanAsset::decode(&b)
                    .map(Arc::new)
                    .map_err(|e| format!("human template {}: {e}", p.display()));
            }
        }
        let list: Vec<String> = tried.iter().map(|p| p.display().to_string()).collect();
        Err(format!(
            "the `human` preset needs its template file human.bin, not found at {} (set {ENV} to its path, or copy data/human/human.bin beside the modeler binary)",
            list.join(", ")
        ))
    })
    .clone()
}

/// Whether the template file can be loaded (tests skip without it).
pub fn available() -> bool {
    load().is_ok()
}
