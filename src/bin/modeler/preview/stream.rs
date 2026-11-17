//! Streaming part entities for the window and the grid tiles: mesh
//! conversion (with tangents), material bakes and decimated copies (distance
//! LODs, tile proxies) run on the `AsyncComputeTaskPool`, so a document
//! switch or an edit never freezes a frame. [`try_bundle`] answers from the
//! caches and queues what is missing; [`poll_streams`] moves finished jobs
//! into the caches each frame. Agent views keep building synchronously
//! (`part_bundle`): their shots must be complete and deterministic.

use std::collections::{HashMap, HashSet};
use std::sync::Weak;

use bevy::camera::visibility::VisibilityRange;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use super::app::{
    PbrCache, PreparedMat, build_mesh, closed_solid, content_hash, fingerprint, finish_material,
    flat_material, material_key, prepare_material, wants_tangents,
};
use crate::prelude::*;

/// Parts over this many triangles get distance LODs in the window.
pub(crate) const LOD_MIN_TRIS: usize = 50_000;
/// A grid tile draws each part with at most this many triangles...
pub(crate) const PROXY_PART_TRIS: usize = 20_000;
/// ...and the whole tile with at most about this many.
pub(crate) const PROXY_TILE_TRIS: usize = 100_000;

/// How finely a streamed part is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Detail {
    /// The window: the full mesh, plus decimated copies swapped by
    /// distance for a heavy part.
    Lod,
    /// A grid tile: a decimated copy of at most this many triangles.
    Proxy(u32),
}

/// What the prep job learned about one part (by `Arc` address; the `Weak`
/// keeps the allocation, so the address isn't reused while it is cached).
struct Prep {
    part: Weak<Part>,
    hash: u64,
    mat_key: Option<u64>,
    tangents: bool,
    /// Half the diagonal of the part's own box, times its largest scale.
    radius: f32,
}

struct PrepOut {
    fp: (usize, usize, usize, u64),
    hash: u64,
    closed: bool,
    mat_key: Option<u64>,
    tangents: bool,
    radius: f32,
    mesh: Option<Mesh>,
}

type LodKey = (u64, u32, bool);

/// Jobs in flight and their finished results that live outside
/// [`PbrCache`].
#[derive(Resource, Default)]
pub(crate) struct Streams {
    preps: HashMap<usize, Prep>,
    prep_jobs: HashMap<usize, (Weak<Part>, Task<PrepOut>)>,
    bake_jobs: HashMap<u64, Task<Option<PreparedMat>>>,
    /// Bakes that gave nothing (failed): those parts stay flat.
    no_bake: HashSet<u64>,
    lods: HashMap<LodKey, (Handle<Mesh>, u64)>,
    lod_jobs: HashMap<LodKey, Task<Mesh>>,
    generation: u64,
    /// Grid tiles not yet rendered from their finished parts.
    pub(crate) pending_grid: usize,
}

/// A streamed part's assets once its mesh exists.
pub(crate) struct Built {
    pub(crate) mesh: Handle<Mesh>,
    pub(crate) material: Handle<StandardMaterial>,
    /// False while the material bake runs (`material` is the flat
    /// placeholder of the part's factors).
    pub(crate) final_material: bool,
    /// Distance LODs: `(mesh, from, to)` camera distances, the full mesh
    /// showing below the first `from`. `None` while they are being built;
    /// empty for a light part.
    pub(crate) lods: Option<Vec<(Handle<Mesh>, f32, f32)>>,
}

/// Moves finished jobs into the caches (a few at a time: adding a big
/// mesh or texture set is cheap, its GPU upload follows in the render
/// world).
pub(crate) fn poll_streams(
    mut streams: ResMut<Streams>,
    mut cache: ResMut<PbrCache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let _profile = crate::preview::profile::span("poll_streams");
    let s = &mut *streams;
    s.generation += 1;
    let generation = cache.generation;
    let mut done = Vec::new();
    for (k, (_, task)) in s.prep_jobs.iter_mut() {
        if let Some(out) = check_ready(task) {
            done.push((*k, out));
        }
    }
    for (k, out) in done {
        let Some((weak, _)) = s.prep_jobs.remove(&k) else {
            continue;
        };
        cache.fingerprints.insert(out.fp, out.hash);
        cache.closed.insert(out.hash, out.closed);
        if let Some(mesh) = out.mesh {
            cache
                .meshes
                .entry((out.hash, out.tangents))
                .or_insert_with(|| (meshes.add(mesh), generation));
        }
        s.preps.insert(
            k,
            Prep {
                part: weak,
                hash: out.hash,
                mat_key: out.mat_key,
                tangents: out.tangents,
                radius: out.radius,
            },
        );
    }
    let mut baked = Vec::new();
    for (k, task) in s.bake_jobs.iter_mut() {
        if let Some(out) = check_ready(task) {
            baked.push((*k, out));
        }
    }
    for (k, out) in baked {
        s.bake_jobs.remove(&k);
        match out {
            Some(p) => {
                let h = finish_material(p, &mut materials, &mut images);
                cache.pbr.insert(k, (h, generation));
            }
            None => {
                s.no_bake.insert(k);
            }
        }
    }
    let mut lods = Vec::new();
    for (k, task) in s.lod_jobs.iter_mut() {
        if let Some(m) = check_ready(task) {
            lods.push((*k, m));
        }
    }
    let g = s.generation;
    for (k, m) in lods {
        s.lod_jobs.remove(&k);
        s.lods.insert(k, (meshes.add(m), g));
    }
    // Forget parts that are gone, and decimated copies nothing asked for
    // in a while.
    if g.is_multiple_of(120) {
        s.preps.retain(|_, p| p.part.strong_count() > 0);
        s.lods.retain(|_, (_, used)| g - *used < 3600);
        if s.no_bake.len() > 4096 {
            s.no_bake.clear();
        }
    }
}

fn spawn_prep(s: &mut Streams, cache: &PbrCache, part: &Arc<Part>) {
    let ptr = Arc::as_ptr(part) as usize;
    if s.prep_jobs.contains_key(&ptr) {
        return;
    }
    let fp = fingerprint(part);
    let tangents = wants_tangents(part);
    // Content already known (an unchanged part respawned): skip hashing
    // and building a mesh that is cached.
    let known = cache.fingerprints.get(&fp).copied();
    let build = known.is_none_or(|h| !cache.meshes.contains_key(&(h, tangents)));
    let closed = known.and_then(|h| cache.closed.get(&h).copied());
    let p = Arc::clone(part);
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let hash = known.unwrap_or_else(|| content_hash(&p));
        let closed = if p.material.pbr.is_some() {
            closed.unwrap_or_else(|| closed_solid(&p))
        } else {
            false
        };
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in &p.verts {
            let v = Vec3::from_array(*v);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        let s = Vec3::from_array(p.scale).abs().max_element();
        let radius = if p.verts.is_empty() {
            0.0
        } else {
            (hi - lo).length() * 0.5 * s
        };
        PrepOut {
            fp,
            hash,
            closed,
            mat_key: material_key(&p, closed),
            tangents,
            radius,
            mesh: build.then(|| build_mesh(&p, tangents)),
        }
    });
    s.prep_jobs.insert(ptr, (Arc::downgrade(part), task));
}

/// The decimated copy of `part` at about `target` triangles, queued when
/// missing.
fn lod_mesh(
    s: &mut Streams,
    part: &Arc<Part>,
    hash: u64,
    tangents: bool,
    target: u32,
) -> Option<Handle<Mesh>> {
    let key = (hash, target, tangents);
    let g = s.generation;
    if let Some((h, used)) = s.lods.get_mut(&key) {
        *used = g;
        return Some(h.clone());
    }
    s.lod_jobs.entry(key).or_insert_with(|| {
        let p = Arc::clone(part);
        AsyncComputeTaskPool::get().spawn(async move {
            let ratio = target as f32 / p.tri_count().max(1) as f32;
            let lo = modeler::simplify::simplify(&p, ratio);
            build_mesh(&lo, tangents)
        })
    });
    None
}

/// The streamed entity's assets for `part`, or `None` while its mesh is
/// still being built (the jobs it needs are queued).
pub(crate) fn try_bundle(
    part: &Arc<Part>,
    detail: Detail,
    s: &mut Streams,
    cache: &mut PbrCache,
    materials: &mut Assets<StandardMaterial>,
) -> Option<Built> {
    let ptr = Arc::as_ptr(part) as usize;
    let Some(prep) = s.preps.get(&ptr) else {
        spawn_prep(s, cache, part);
        return None;
    };
    let (hash, tangents, mat_key, radius) = (prep.hash, prep.tangents, prep.mat_key, prep.radius);
    let generation = cache.generation;
    let Some(full) = cache.meshes.get_mut(&(hash, tangents)).map(|(h, used)| {
        *used = generation;
        h.clone()
    }) else {
        // Evicted since: build it again.
        s.preps.remove(&ptr);
        spawn_prep(s, cache, part);
        return None;
    };
    let (material, final_material) = match mat_key {
        Some(k) => match cache.pbr.get_mut(&k) {
            Some((h, used)) => {
                *used = generation;
                (h.clone(), true)
            }
            None => {
                let failed = s.no_bake.contains(&k);
                if !failed && !s.bake_jobs.contains_key(&k) {
                    let p = Arc::clone(part);
                    let task =
                        AsyncComputeTaskPool::get().spawn(async move { prepare_material(&p) });
                    s.bake_jobs.insert(k, task);
                }
                (flat_material(part, materials, cache), failed)
            }
        },
        None => (flat_material(part, materials, cache), true),
    };
    let tris = part.tri_count() as usize;
    match detail {
        Detail::Proxy(target) => {
            let mesh = if tris > target as usize {
                lod_mesh(s, part, hash, tangents, target)?
            } else {
                full
            };
            Some(Built {
                mesh,
                material,
                final_material,
                lods: Some(Vec::new()),
            })
        }
        Detail::Lod => {
            let lods = if tris > LOD_MIN_TRIS {
                // A quarter of the triangles from 6 part radii away, a
                // sixteenth from 20: about where the dropped detail is
                // under a pixel or two at the window's field of view.
                let r = radius.max(0.01);
                let levels = [
                    ((tris / 4) as u32, r * 6.0, r * 20.0),
                    ((tris / 16) as u32, r * 20.0, 1.0e9),
                ];
                let mut v = Vec::new();
                let mut ready = true;
                for (t, from, to) in levels {
                    match lod_mesh(s, part, hash, tangents, t) {
                        Some(h) => v.push((h, from, to)),
                        None => ready = false,
                    }
                }
                ready.then_some(v)
            } else {
                Some(Vec::new())
            };
            Some(Built {
                mesh: full,
                material,
                final_material,
                lods,
            })
        }
    }
}

/// Marks a window LOD copy (a child of its part's entity).
#[derive(Component)]
pub(crate) struct PartLod;

/// Gives a window part entity its distance LODs: the full mesh shows
/// nearer than the first LOD's distance, each copy over its own range.
pub(crate) fn attach_lods(
    commands: &mut Commands,
    e: Entity,
    material: &Handle<StandardMaterial>,
    lods: &[(Handle<Mesh>, f32, f32)],
) {
    let Some(first) = lods.first() else {
        return;
    };
    commands.entity(e).insert(VisibilityRange {
        start_margin: 0.0..0.0,
        end_margin: first.1..first.1,
        use_aabb: true,
    });
    for (mesh, from, to) in lods {
        let child = commands
            .spawn((
                PartLod,
                // Out-of-range copies still cast in Bevy's directional
                // shadow pass here (they darkened the full mesh up close).
                bevy::light::NotShadowCaster,
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::IDENTITY,
                VisibilityRange {
                    start_margin: *from..*from,
                    end_margin: *to..*to,
                    use_aabb: true,
                },
            ))
            .id();
        commands.entity(e).add_child(child);
    }
}
