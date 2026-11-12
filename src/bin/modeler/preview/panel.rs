use crate::prelude::*;

/// The bottom UI line showing the pin's typed-so-far comment.
#[derive(Component)]
pub(crate) struct TypingHud;

/// A selected part's own copy of its (shared) material, and the shared one
/// to put back when it is deselected.
#[derive(Component)]
pub(crate) struct Highlight(Handle<StandardMaterial>);

/// The outliner's selected parts pulse in the 3D view. Materials are shared
/// between parts, so a selected part pulses on its own copy.
pub(crate) fn highlight_selected(
    mut commands: Commands,
    time: Res<Time>,
    outline: Res<OutlinePane>,
    mut parts: Query<
        (
            Entity,
            &Name,
            &mut MeshMaterial3d<StandardMaterial>,
            Option<&Highlight>,
        ),
        With<PreviewPart>,
    >,
    lit: Query<(), With<Highlight>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let sel: &[String] = if outline.visible {
        &outline.selected_parts
    } else {
        &[]
    };
    if sel.is_empty() && lit.is_empty() {
        return;
    }
    let pulse = 0.35 + 0.25 * (time.elapsed_secs() * 5.0).sin();
    for (e, name, mut mat, lit) in &mut parts {
        let selected = sel.iter().any(|s| s == name.as_str());
        match (selected, lit) {
            (true, _) => {
                if lit.is_none() {
                    let Some(m) = materials.get(&mat.0).cloned() else {
                        continue;
                    };
                    if m.emissive_texture.is_some() {
                        continue;
                    }
                    let own = materials.add(m);
                    commands.entity(e).insert(Highlight(mat.0.clone()));
                    mat.0 = own;
                }
                if let Some(mut m) = materials.get_mut(&mat.0) {
                    m.emissive = LinearRgba::rgb(pulse, pulse * 0.8, 0.1);
                }
            }
            (false, Some(h)) => {
                materials.remove(&mat.0);
                mat.0 = h.0.clone();
                commands.entity(e).remove::<Highlight>();
            }
            (false, None) => {}
        }
    }
}
