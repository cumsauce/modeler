//! Clip playback for exported characters: one call to play a clip by name
//! with a crossfade ([`ModelerAnimator`]), the clips' root motion applied to
//! the entity ([`ModelerRootMotion`]) and their events as Bevy messages
//! ([`ModelerClipEvent`]).
//!
//! The exporter writes each skeleton's clips as `extras.clips` on the
//! skeleton's root node (`{name: {loop, duration, root_motion?, events?}}`:
//! Bevy's loader does not expose animation extras), and mirrors them on each
//! animation. Everything here is driven by that table plus the
//! [`AnimationPlayer`]s Bevy's loader puts on the glTF's animation roots:
//! a rigged character has one player on its skeleton root and one on every
//! other root node that a clip animates (a body's morph weights), so
//! [`ModelerClips`] sets up and plays all of them together.
//!
//! ```ignore
//! // Spawn:
//! let (clips, _) = ModelerClips::from_gltf(gltf, &mut graphs);
//! commands.spawn((WorldAssetRoot(scene), clips, ModelerRootMotion::default()));
//! // Later, from any system:
//! fn go(mut anim: ModelerAnimator, me: Single<Entity, With<ModelerClips>>) {
//!     anim.play(*me, "walk", Duration::from_millis(200));
//! }
//! fn footsteps(mut events: MessageReader<ModelerClipEvent>) {
//!     for e in events.read() { if e.name == "footstep_l" { /* ... */ } }
//! }
//! ```

use std::collections::HashMap;
use std::time::Duration;

use bevy::animation::transition::AnimationTransitions;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

/// One clip's playback metadata (the animation's `extras`).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipInfo {
    pub name: String,
    pub duration: f32,
    pub looping: bool,
    pub root_motion: Option<RootMotionTrack>,
    pub events: Vec<ClipEventDef>,
}

/// The travel a clip removed from its root bone, sampled from t = 0.
#[derive(Clone, Debug, PartialEq)]
pub struct RootMotionTrack {
    /// Which world axes travel (`"xz"`).
    pub axes: [bool; 3],
    pub yaw_on: bool,
    /// Total travel over one cycle, in the clip's start frame.
    pub delta: Vec3,
    pub yaw_degrees: f32,
    /// `(time, position, yaw degrees)`, cumulative from the clip's start.
    pub samples: Vec<(f32, Vec3, f32)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClipEventDef {
    pub time: f32,
    pub name: String,
    pub data: Option<serde_json::Value>,
}

/// A skeleton's clips, from its root node's `extras.clips`.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct ModelerClipTable(pub Vec<ClipInfo>);

impl ModelerClipTable {
    pub fn get(&self, name: &str) -> Option<&ClipInfo> {
        self.0.iter().find(|c| c.name == name)
    }
}

fn vec3_at(v: &serde_json::Value, from: usize) -> Option<Vec3> {
    Some(Vec3::new(
        v.get(from)?.as_f64()? as f32,
        v.get(from + 1)?.as_f64()? as f32,
        v.get(from + 2)?.as_f64()? as f32,
    ))
}

/// Parses one animation's extras (`loop`, `duration`, `root_motion`, `events`).
pub fn clip_info_of(name: &str, x: &serde_json::Value) -> ClipInfo {
    let duration = x.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0) as f32;
    let root_motion = x.get("root_motion").and_then(|r| {
        let axes = r.get("axes").and_then(|a| a.as_str()).unwrap_or("xz");
        let samples: Vec<(f32, Vec3, f32)> = r
            .get("samples")?
            .as_array()?
            .iter()
            .filter_map(|s| {
                Some((
                    s.get(0)?.as_f64()? as f32,
                    vec3_at(s, 1)?,
                    s.get(4)?.as_f64()? as f32,
                ))
            })
            .collect();
        (!samples.is_empty()).then(|| RootMotionTrack {
            axes: [axes.contains('x'), axes.contains('y'), axes.contains('z')],
            yaw_on: r.get("yaw").and_then(|y| y.as_bool()).unwrap_or(false),
            delta: r
                .get("delta")
                .and_then(|d| vec3_at(d, 0))
                .unwrap_or(Vec3::ZERO),
            yaw_degrees: r.get("yaw_degrees").and_then(|y| y.as_f64()).unwrap_or(0.0) as f32,
            samples,
        })
    });
    let mut events: Vec<ClipEventDef> = x
        .get("events")
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    Some(ClipEventDef {
                        time: e.get("time")?.as_f64()? as f32,
                        name: e.get("name")?.as_str()?.to_string(),
                        data: e.get("data").cloned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    events.sort_by(|a, b| a.time.total_cmp(&b.time));
    ClipInfo {
        name: name.to_string(),
        duration,
        looping: x.get("loop").and_then(|l| l.as_bool()).unwrap_or(true),
        root_motion,
        events,
    }
}

/// `extras.clips` of a skeleton root node.
pub fn clip_table_of(v: &serde_json::Value) -> Option<ModelerClipTable> {
    let m = v.get("clips")?.as_object()?;
    let mut t: Vec<ClipInfo> = m.iter().map(|(k, x)| clip_info_of(k, x)).collect();
    t.sort_by(|a, b| a.name.cmp(&b.name));
    Some(ModelerClipTable(t))
}

impl RootMotionTrack {
    /// Position and yaw (degrees) travelled `t` seconds into one cycle,
    /// interpolated between samples and held past the ends.
    pub fn at(&self, t: f32) -> (Vec3, f32) {
        let s = &self.samples;
        let Some(first) = s.first() else {
            return (Vec3::ZERO, 0.0);
        };
        if t <= first.0 {
            return (first.1, first.2);
        }
        let i = s.partition_point(|x| x.0 <= t);
        let Some(b) = s.get(i) else {
            let l = &s[s.len() - 1];
            return (l.1, l.2);
        };
        let a = &s[i - 1];
        let f = ((t - a.0) / (b.0 - a.0).max(1e-9)).clamp(0.0, 1.0);
        (a.1.lerp(b.1, f), a.2 + (b.2 - a.2) * f)
    }

    /// Moves `tf` by the travel between two times of one cycle. The travel
    /// is in the clip's start frame, which `tf`'s rotation has been turned
    /// by the yaw so far, so the step is carried back into it first.
    pub fn apply(&self, tf: &mut Transform, from: f32, to: f32, weight: f32) {
        let (p0, y0) = self.at(from);
        let (p1, y1) = self.at(to);
        let step = (p1 - p0) * weight;
        let start_frame = tf.rotation * Quat::from_rotation_y((-y0).to_radians());
        tf.translation += start_frame * step;
        if self.yaw_on {
            tf.rotation = (tf.rotation * Quat::from_rotation_y(((y1 - y0) * weight).to_radians()))
                .normalize();
        }
    }
}

/// Where a clip's playback was last frame: `(seek time, completions)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playhead {
    pub seek: f32,
    pub completions: u32,
}

/// The stretches of one cycle a playhead crossed between two frames, as
/// `(from, to, include_from)`: wraps split in two, whole loops in between
/// are `(0, duration, true)`; a clip that is not looping stops at its end.
pub fn crossed(
    duration: f32,
    looping: bool,
    prev: Option<Playhead>,
    now: Playhead,
) -> Vec<(f32, f32, bool)> {
    let end = |s: f32| s.clamp(0.0, duration);
    let Some(p) = prev else {
        return vec![(0.0, end(now.seek), true)];
    };
    if !looping {
        return if end(now.seek) > end(p.seek) {
            vec![(end(p.seek), end(now.seek), false)]
        } else {
            vec![]
        };
    }
    if now.completions <= p.completions {
        return if now.seek > p.seek {
            vec![(p.seek, now.seek, false)]
        } else {
            vec![]
        };
    }
    let mut v = vec![(p.seek, duration, false)];
    for _ in 1..(now.completions - p.completions).min(8) {
        v.push((0.0, duration, true));
    }
    v.push((0.0, now.seek, true));
    v
}

/// Message fired when playback crosses a clip event's time.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct ModelerClipEvent {
    /// The entity carrying [`ModelerClips`] (the scene root).
    pub entity: Entity,
    pub clip: String,
    pub name: String,
    pub time: f32,
    pub data: Option<serde_json::Value>,
}

/// Clip names -> animation graph nodes of one exported character. Put it on
/// the scene root: the plugin gives every [`AnimationPlayer`] below it the
/// graph and an [`AnimationTransitions`].
#[derive(Component, Clone, Debug)]
pub struct ModelerClips {
    pub graph: Handle<AnimationGraph>,
    pub nodes: HashMap<String, AnimationNodeIndex>,
}

impl ModelerClips {
    /// A graph with one node per named animation of the glTF.
    pub fn from_gltf(gltf: &bevy::gltf::Gltf, graphs: &mut Assets<AnimationGraph>) -> Self {
        let mut names: Vec<(&str, Handle<AnimationClip>)> = gltf
            .named_animations
            .iter()
            .map(|(k, v)| (&**k, v.clone()))
            .collect();
        names.sort_by(|a, b| a.0.cmp(b.0));
        let (graph, idx) = AnimationGraph::from_clips(names.iter().map(|n| n.1.clone()));
        Self {
            graph: graphs.add(graph),
            nodes: names
                .iter()
                .zip(idx)
                .map(|(n, i)| (n.0.to_string(), i))
                .collect(),
        }
    }

    pub fn node(&self, name: &str) -> Option<AnimationNodeIndex> {
        self.nodes.get(name).copied()
    }

    pub fn name_of(&self, node: AnimationNodeIndex) -> Option<&str> {
        self.nodes
            .iter()
            .find(|(_, i)| **i == node)
            .map(|(n, _)| n.as_str())
    }

    /// Clip names, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.nodes.keys().map(|k| k.as_str()).collect();
        v.sort();
        v
    }
}

/// Opts the entity into root motion: its transform is moved by the playing
/// clips' `extras.root_motion` (weighted while crossfading), wrapping on
/// loops. Put it on the scene root (the entity with [`ModelerClips`]).
#[derive(Component, Clone, Debug)]
pub struct ModelerRootMotion {
    pub enabled: bool,
}

impl Default for ModelerRootMotion {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Plays clips on a [`ModelerClips`] entity: every player below it, with
/// a crossfade, honouring each clip's `extras.loop`. Playing a sway's own
/// baked clip turns its live simulation off, and any other clip turns it on.
#[derive(SystemParam)]
pub struct ModelerAnimator<'w, 's> {
    players: Query<
        'w,
        's,
        (
            &'static mut AnimationPlayer,
            &'static mut AnimationTransitions,
        ),
    >,
    clips: Query<'w, 's, &'static ModelerClips>,
    tables: Query<'w, 's, &'static ModelerClipTable>,
    children: Query<'w, 's, &'static Children>,
    sways: Query<'w, 's, &'static mut crate::Sway>,
}

impl ModelerAnimator<'_, '_> {
    fn below(&self, root: Entity) -> Vec<Entity> {
        std::iter::once(root)
            .chain(self.children.iter_descendants(root))
            .collect()
    }

    /// Starts `clip` on every player under `root`, fading out whatever played
    /// over `fade`. A looping clip repeats; others play once. False when the
    /// root has no [`ModelerClips`] or no such clip.
    pub fn play(&mut self, root: Entity, clip: &str, fade: Duration) -> bool {
        let Some(node) = self.clips.get(root).ok().and_then(|c| c.node(clip)) else {
            return false;
        };
        let all = self.below(root);
        let looping = all
            .iter()
            .find_map(|e| self.tables.get(*e).ok()?.get(clip))
            .is_none_or(|c| c.looping);
        for e in all {
            // A baked sway clip replaces the live sway while it plays.
            if let Ok(mut s) = self.sways.get_mut(e) {
                s.live = s.rig.clip.as_deref() != Some(clip);
            }
            if let Ok((mut p, mut t)) = self.players.get_mut(e) {
                let a = t.play(&mut p, node, fade);
                if looping {
                    a.repeat();
                }
            }
        }
        true
    }

    /// The clips playing under `root` with their weights.
    pub fn playing(&self, root: Entity) -> Vec<(String, f32)> {
        let Ok(c) = self.clips.get(root) else {
            return Vec::new();
        };
        let mut out: Vec<(String, f32)> = Vec::new();
        for e in self.below(root) {
            if let Ok((p, _)) = self.players.get(e) {
                for (i, a) in p.playing_animations() {
                    if let Some(n) = c.name_of(*i)
                        && !out.iter().any(|o| o.0 == n)
                    {
                        out.push((n.to_string(), a.weight()));
                    }
                }
            }
        }
        out
    }
}

/// Gives every new [`AnimationPlayer`] below a [`ModelerClips`] its graph
/// and transitions.
pub(crate) fn setup_players(
    mut commands: Commands,
    players: Query<Entity, (With<AnimationPlayer>, Without<AnimationTransitions>)>,
    clips: Query<&ModelerClips>,
    parents: Query<&ChildOf>,
) {
    for e in &players {
        let Some(c) = std::iter::once(e)
            .chain(parents.iter_ancestors(e))
            .find_map(|a| clips.get(a).ok())
        else {
            continue;
        };
        commands.entity(e).insert((
            AnimationGraphHandle(c.graph.clone()),
            AnimationTransitions::new(),
        ));
    }
}

/// Cursors of the playing animations, per scene root and graph node.
#[derive(Resource, Default)]
pub(crate) struct Playheads(HashMap<(Entity, AnimationNodeIndex), Playhead>);

/// Applies root motion and fires clip events from what the players played.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_clip_cursors(
    mut heads: ResMut<Playheads>,
    players: Query<(Entity, &AnimationPlayer)>,
    parents: Query<&ChildOf>,
    clips: Query<&ModelerClips>,
    tables: Query<&ModelerClipTable>,
    children: Query<&Children>,
    motion: Query<&ModelerRootMotion>,
    mut transforms: Query<&mut Transform>,
    mut events: MessageWriter<ModelerClipEvent>,
) {
    let mut seen: Vec<(Entity, AnimationNodeIndex)> = Vec::new();
    for (pe, player) in &players {
        let Some(root) = std::iter::once(pe)
            .chain(parents.iter_ancestors(pe))
            .find(|a| clips.contains(*a))
        else {
            continue;
        };
        let Ok(c) = clips.get(root) else { continue };
        for (node, a) in player.playing_animations() {
            let key = (root, *node);
            // Every player of a character plays the same clips in step.
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let Some(name) = c.name_of(*node) else {
                continue;
            };
            let Some(info) = std::iter::once(root)
                .chain(children.iter_descendants(root))
                .find_map(|e| tables.get(e).ok()?.get(name))
            else {
                continue;
            };
            let now = Playhead {
                seek: a.seek_time(),
                completions: a.completions(),
            };
            let prev = heads.0.insert(key, now);
            if a.speed() <= 0.0 || a.is_paused() {
                continue;
            }
            let spans = crossed(info.duration, info.looping, prev, now);
            if let Some(rm) = info.root_motion.as_ref()
                && motion.get(root).is_ok_and(|m| m.enabled)
                && let Ok(mut tf) = transforms.get_mut(root)
            {
                for (from, to, _) in &spans {
                    rm.apply(&mut tf, *from, *to, a.weight());
                }
            }
            for (from, to, inc) in &spans {
                for ev in &info.events {
                    let hit = (ev.time > *from || (*inc && ev.time >= *from)) && ev.time <= *to;
                    if hit {
                        events.write(ModelerClipEvent {
                            entity: root,
                            clip: name.to_string(),
                            name: ev.name.clone(),
                            time: ev.time,
                            data: ev.data.clone(),
                        });
                    }
                }
            }
        }
    }
    // Forget animations that stopped.
    heads.0.retain(|k, _| seen.contains(k));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> RootMotionTrack {
        let x = serde_json::json!({"loop": true, "duration": 1.0,
            "root_motion": {"axes": "xz", "yaw": true, "delta": [0, 0, 1.2], "yaw_degrees": 90.0,
              "samples": [[0, 0, 0, 0, 0], [0.5, 0, 0, 0.6, 45], [1, 0, 0, 1.2, 90]]},
            "events": [{"time": 0.5, "name": "b"}, {"time": 0.0, "name": "a", "data": {"v": 1}}]});
        clip_info_of("step", &x)
            .root_motion
            .unwrap_or_else(|| panic!("no track"))
    }

    #[test]
    fn root_motion_parses_and_interpolates() {
        let t = track();
        assert_eq!(t.axes, [true, false, true]);
        let (p, y) = t.at(0.25);
        assert!((p.z - 0.3).abs() < 1e-5 && (y - 22.5).abs() < 1e-4);
        assert_eq!(t.at(2.0).0.z, 1.2);
    }

    #[test]
    fn crossed_spans_wrap_and_stop() {
        let a = Playhead {
            seek: 0.9,
            completions: 0,
        };
        let b = Playhead {
            seek: 0.2,
            completions: 1,
        };
        assert_eq!(
            crossed(1.0, true, Some(a), b),
            vec![(0.9, 1.0, false), (0.0, 0.2, true)]
        );
        assert_eq!(crossed(1.0, true, None, a), vec![(0.0, 0.9, true)]);
        // A finished one-shot holds its end (seek runs past it).
        let done = Playhead {
            seek: 1.3,
            completions: 1,
        };
        assert_eq!(crossed(1.0, false, Some(a), done), vec![(0.9, 1.0, false)]);
        assert!(crossed(1.0, false, Some(done), done).is_empty());
    }

    #[test]
    fn travel_accumulates_across_loops_and_turns() {
        let t = track();
        let mut tf = Transform::default();
        // Three frames, the second wraps the loop.
        let mut prev: Option<Playhead> = None;
        for (seek, completions) in [(0.4, 0), (0.9, 0), (0.3, 1), (0.8, 1)] {
            let now = Playhead { seek, completions };
            for (f, to, _) in crossed(1.0, true, prev, now) {
                t.apply(&mut tf, f, to, 1.0);
            }
            prev = Some(now);
        }
        // 0.8 s of a second cycle after one full one: 1.2 forward along the
        // original heading, then 0.96 along the heading turned 90 degrees.
        let first = Vec3::new(0.0, 0.0, 1.2);
        let turned = Quat::from_rotation_y(90f32.to_radians()) * Vec3::new(0.0, 0.0, 0.96);
        let want = first + turned;
        assert!(
            (tf.translation - want).length() < 1e-4,
            "{:?} vs {want:?}",
            tf.translation
        );
        let yaw = tf.rotation.to_euler(EulerRot::YXZ).0.to_degrees();
        assert!((yaw - (90.0 + 72.0)).abs() < 1e-2, "{yaw}");
    }

    #[test]
    fn events_fire_once_per_crossing() {
        let x = serde_json::json!({"events": [{"time": 0.0, "name": "a"}, {"time": 0.5, "name": "b"}], "duration": 1.0});
        let info = clip_info_of("c", &x);
        let mut fired: Vec<String> = Vec::new();
        let mut prev: Option<Playhead> = None;
        for (seek, completions) in [(0.0, 0), (0.3, 0), (0.6, 0), (0.1, 1), (0.6, 1)] {
            let now = Playhead { seek, completions };
            for (from, to, inc) in crossed(info.duration, true, prev, now) {
                for e in &info.events {
                    if (e.time > from || (inc && e.time >= from)) && e.time <= to {
                        fired.push(e.name.clone());
                    }
                }
            }
            prev = Some(now);
        }
        assert_eq!(fired, ["a", "b", "a", "b"]);
    }
}
