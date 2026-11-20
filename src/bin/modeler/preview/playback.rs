//! Clip playback in the preview window: rigged parts are posed on the CPU
//! every frame by the same evaluator agent views, verify and export use
//! (`modeler::anim`), written into the window's meshes. The agent drives it
//! with `set_camera(play: {...})`; the human with P (play / pause), [ and ]
//! (previous / next clip), comma and period (step a frame while paused).

use crate::prelude::*;

/// What the window plays.
#[derive(Resource)]
pub(crate) struct Playback {
    pub(crate) clip: Option<String>,
    pub(crate) time: f32,
    pub(crate) speed: f32,
    pub(crate) playing: bool,
    /// The window's meshes are posed (not at rest).
    pub(crate) posed: bool,
    /// Model version and time last written, to skip identical frames.
    last: Option<(u64, u32)>,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            clip: None,
            time: 0.0,
            speed: 1.0,
            playing: false,
            posed: false,
            last: None,
        }
    }
}

/// The old playback HUD line (spawned in app.rs; kept hidden).
#[derive(Component)]
pub(crate) struct PlaybackHud;

/// A playback request from the agent (`set_camera` `play`).
#[derive(Clone, Debug, Default)]
pub(crate) struct PlayRequest {
    /// The clip; empty string stops playback (back to rest).
    pub(crate) clip: Option<String>,
    pub(crate) time: Option<f32>,
    pub(crate) speed: Option<f32>,
    pub(crate) paused: Option<bool>,
}

pub(crate) fn clip_names(model: &modeler::Model) -> Vec<String> {
    let mut names: Vec<String> = model
        .ops
        .iter()
        .filter_map(|o| match o {
            modeler::Op::Clip { name, .. } | modeler::Op::Sway { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    // Prefab instances bring their clips as `inst.<clip>`.
    if model
        .ops
        .iter()
        .any(|o| matches!(o, modeler::Op::Prefab { .. }))
    {
        for n in modeler::anim::clip_names(model) {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names
}

impl Playback {
    /// Applies an agent request; returns a description or an error.
    pub(crate) fn request(
        &mut self,
        model: &modeler::Model,
        r: &PlayRequest,
    ) -> Result<String, String> {
        let names = clip_names(model);
        match r.clip.as_deref() {
            Some("") => {
                self.clip = None;
                self.playing = false;
                return Ok("playback stopped (rest pose)".into());
            }
            Some(c) => {
                // A pose (a static clip) or a mocap clip plays by name too.
                if !names.iter().any(|n| n == c) && modeler::anim::clip(model, Some(c)).is_err() {
                    return Err(format!(
                        "play: no clip `{c}` (clips: {})",
                        if names.is_empty() {
                            "none".into()
                        } else {
                            names.join(", ")
                        }
                    ));
                }
                self.clip = Some(c.to_string());
            }
            None if self.clip.is_none() => {
                let Some(first) = names.first() else {
                    return Err("play: the model has no clip".into());
                };
                self.clip = Some(first.clone());
            }
            None => {}
        }
        if let Some(t) = r.time {
            self.time = t;
        }
        if let Some(s) = r.speed {
            self.speed = s;
        }
        self.playing = !r.paused.unwrap_or(false);
        Ok(format!(
            "playing `{}` from t = {:.3} at {}x{}",
            self.clip.clone().unwrap_or_default(),
            self.time,
            self.speed,
            if self.playing { "" } else { " (paused)" }
        ))
    }
}

pub(crate) fn playback_keys(
    keys: Res<ButtonInput<KeyCode>>,
    typing: Res<PinTyping>,
    modeler: Res<Modeler>,
    mut pb: ResMut<Playback>,
) {
    if typing.open {
        return;
    }
    let names = clip_names(&modeler.session.model);
    if names.is_empty() {
        return;
    }
    let cycle = |pb: &mut Playback, step: isize| {
        let i = pb
            .clip
            .as_ref()
            .and_then(|c| names.iter().position(|n| n == c))
            .map_or(0, |i| {
                (i as isize + step).rem_euclid(names.len() as isize) as usize
            });
        pb.clip = Some(names[i].clone());
        pb.time = 0.0;
    };
    if keys.just_pressed(KeyCode::KeyP) {
        if pb.clip.is_none() {
            cycle(&mut pb, 0);
            pb.playing = true;
        } else {
            pb.playing = !pb.playing;
        }
    }
    if keys.just_pressed(KeyCode::BracketRight) {
        cycle(&mut pb, 1);
    }
    if keys.just_pressed(KeyCode::BracketLeft) {
        cycle(&mut pb, -1);
    }
    if pb.clip.is_some() && !pb.playing {
        if keys.just_pressed(KeyCode::Period) {
            pb.time += 1.0 / 30.0;
        }
        if keys.just_pressed(KeyCode::Comma) {
            pb.time -= 1.0 / 30.0;
        }
    }
}

/// Poses the window's rigged parts at the playback time.
pub(crate) fn play_animation(
    time: Res<Time>,
    modeler: Res<Modeler>,
    mut pb: ResMut<Playback>,
    mut built: ResMut<SceneBuilt>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut parts: Query<(&Name, &Mesh3d, &mut Transform), With<PreviewPart>>,
    mut hud: Query<(&Text, &mut Visibility), With<PlaybackHud>>,
) {
    let model = &modeler.session.model;
    let clip = pb
        .clip
        .clone()
        .and_then(|c| modeler::anim::clip(model, Some(&c)).ok());
    // The Animate dock (`timeline.rs`) replaced the old HUD line.
    if let Ok((_, mut vis)) = hud.single_mut() {
        vis.set_if_neq(Visibility::Hidden);
    }
    let Some((c, skel)) = clip else {
        if pb.clip.is_some() {
            pb.clip = None; // the clip went away
            pb.playing = false;
        }
        if pb.posed {
            // Back to rest: rebuild the scene from the model.
            built.version = None;
            pb.posed = false;
            pb.last = None;
        }
        return;
    };
    if pb.playing {
        pb.time += time.delta_secs() * pb.speed;
        if c.looping {
            pb.time = pb.time.rem_euclid(c.duration);
        } else if pb.time > c.duration {
            pb.time = c.duration;
            pb.playing = false;
        }
    }
    let t = c.wrap(pb.time);
    let stamp = (model.version, t.to_bits());
    if pb.last == Some(stamp) && pb.posed {
        return;
    }
    pb.last = Some(stamp);
    let pose = c.sample(&skel, t);
    let mats = skel.skin_matrices(&pose);
    let weights = c.morph_weights(t);
    for (name, mesh, mut tf) in &mut parts {
        let Some(p) = model.parts.iter().find(|p| p.name == name.as_str()) else {
            continue;
        };
        let rig = p.rig.as_ref().filter(|b| b.skeleton == skel.name);
        // Morph weights apply before skinning; a part keyed in this clip
        // is rewritten even at weight 0 (back to its base shape).
        let keyed = weights.iter().any(|w| w.0 == p.name);
        if rig.is_none() && !keyed {
            continue;
        }
        let Some(mut m) = meshes.get_mut(&mesh.0) else {
            continue;
        };
        if m.count_vertices() != p.verts.len() {
            continue; // a proxy mesh (simplified): its vertices don't line up
        }
        let w = modeler::anim::part_weights(&weights, &p.name);
        let morphed = modeler::anim::skin::morphed(p, &w);
        let (mut verts, mut normals) = (Vec::new(), Vec::new());
        match rig {
            Some(rig) => {
                // Positions and normals straight into the mesh: no part
                // clone unless morphed.
                let src = morphed.map(|(verts, normals)| modeler::Part {
                    verts,
                    normals,
                    ..p.as_ref().clone()
                });
                let src = src.as_ref().unwrap_or(p);
                modeler::anim::skin::pose_into(src, rig, &mats, &mut verts, &mut normals);
                *tf = Transform::IDENTITY;
            }
            None => {
                // Morphed only: the part's own space, its transform kept.
                (verts, normals) = morphed.unwrap_or_else(|| (p.verts.clone(), p.normals.clone()));
            }
        }
        let has_normals = normals.len() == verts.len();
        m.insert_attribute(Mesh::ATTRIBUTE_POSITION, verts);
        if has_normals {
            m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        }
    }
    pb.posed = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(items: serde_json::Value) -> modeler::Model {
        let mut s = modeler::commands::Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!("items")
        };
        s.apply_values(None, items)
            .unwrap_or_else(|e| panic!("{e}"));
        s.model
    }

    #[test]
    fn plays_clips_poses_and_layers_by_name() {
        let m = model(serde_json::json!([
            {"type": "skeleton", "name": "rig", "bones": {
                "a": {"head": [0, 0, 0], "tail": [0, 1, 0]},
                "b": {"parent": "a", "head": [0, 1, 0], "tail": [0, 2, 0]}}},
            {"type": "clip", "name": "wave", "skeleton": "rig", "duration": 1,
             "tracks": {"b": {"rotate": [[0, {"z": 0}], [0.5, {"z": 40}], [1, {"z": 0}]]}}},
            {"type": "clip", "name": "nod", "skeleton": "rig", "duration": 1, "over": "wave",
             "additive": true, "tracks": {"a": {"rotate": [[0, {"x": 0}], [0.5, {"x": 10}], [1, {"x": 0}]]}}},
            {"type": "pose", "name": "bent", "skeleton": "rig", "bones": {"b": {"rotate": {"z": 70}}}}
        ]));
        let mut pb = Playback::default();
        for name in ["wave", "nod", "bent"] {
            let r = PlayRequest {
                clip: Some(name.into()),
                ..Default::default()
            };
            let msg = pb.request(&m, &r).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(msg.contains(name), "{msg}");
            assert_eq!(pb.clip.as_deref(), Some(name));
            // What play_animation then samples.
            let (c, k) = modeler::anim::clip(&m, Some(name)).unwrap_or_else(|e| panic!("{e}"));
            assert!(c.sample(&k, c.wrap(0.5)).iter().all(|p| p.1.is_finite()));
        }
        let bad = PlayRequest {
            clip: Some("nope".into()),
            ..Default::default()
        };
        assert!(pb.request(&m, &bad).is_err());
        // Stopping goes back to rest.
        let stop = PlayRequest {
            clip: Some(String::new()),
            ..Default::default()
        };
        assert!(pb.request(&m, &stop).is_ok() && pb.clip.is_none());
    }
}
