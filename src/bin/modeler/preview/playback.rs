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

/// The playback HUD line (bottom left).
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

fn clip_names(model: &modeler::Model) -> Vec<String> {
    model
        .ops
        .iter()
        .filter_map(|o| match o {
            modeler::Op::Clip { name, .. } | modeler::Op::Sway { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
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
                if !names.iter().any(|n| n == c) {
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
    mut hud: Query<(&mut Text, &mut Visibility), With<PlaybackHud>>,
) {
    let model = &modeler.session.model;
    let clip = pb
        .clip
        .clone()
        .and_then(|c| modeler::anim::clip(model, Some(&c)).ok());
    let hud_line = match (&clip, clip_names(model).is_empty()) {
        (_, true) => String::new(),
        (None, false) => "P play  [ ] clip".to_string(),
        (Some((c, _)), _) => format!(
            "{} {}  {:.2} / {:.2} s{}   P {}  [ ] clip{}",
            if pb.playing { ">" } else { "||" },
            c.name,
            c.wrap(pb.time),
            c.duration,
            if (pb.speed - 1.0).abs() > 1e-3 {
                format!("  {}x", pb.speed)
            } else {
                String::new()
            },
            if pb.playing { "pause" } else { "play" },
            if pb.playing { "" } else { "  , . step" }
        ),
    };
    if let Ok((mut text, mut vis)) = hud.single_mut() {
        *vis = if hud_line.is_empty() {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        if text.0 != hud_line {
            text.0 = hud_line;
        }
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
    let posed = modeler::anim::posed_parts(model, &c, &skel, t);
    for (name, mesh, mut tf) in &mut parts {
        let Some(p) = posed
            .iter()
            .find(|p| p.name == name.as_str() && p.rig.is_some())
        else {
            continue;
        };
        if p.rig.as_ref().is_some_and(|b| b.skeleton != skel.name) {
            continue;
        }
        let Some(mut m) = meshes.get_mut(&mesh.0) else {
            continue;
        };
        m.insert_attribute(Mesh::ATTRIBUTE_POSITION, p.verts.clone());
        if p.normals.len() == p.verts.len() {
            m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, p.normals.clone());
        }
        *tf = Transform::IDENTITY;
    }
    pb.posed = true;
}
