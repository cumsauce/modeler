use crate::prelude::*;

/// The bottom UI line showing the pin's typed-so-far comment.
#[derive(Component)]
pub(crate) struct TypingHud;

/// A selected part's own copy of its (shared) material, and the shared one
/// to put back when it is deselected.
#[derive(Component)]
pub(crate) struct Highlight(Handle<StandardMaterial>);

/// The selection (a 3D click, or the left pane's rows) pulses in the 3D
/// view and the part under the cursor glows faintly. Materials are shared
/// between parts, so a lit part glows on its own copy, and gets the shared
/// one back after.
#[allow(clippy::type_complexity)]
pub(crate) fn highlight_selected(
    mut commands: Commands,
    time: Res<Time>,
    outline: Res<OutlinePane>,
    wb: Res<Workbench>,
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
    let sel = |n: &str| {
        wb.selected.as_deref() == Some(n)
            || (outline.visible && outline.selected_parts.iter().any(|s| s == n))
    };
    let hover = wb.hover.as_deref();
    if wb.selected.is_none()
        && outline.selected_parts.is_empty()
        && hover.is_none()
        && lit.is_empty()
    {
        return;
    }
    let pulse = 0.35 + 0.25 * (time.elapsed_secs() * 5.0).sin();
    for (e, name, mut mat, lit) in &mut parts {
        let glow = if sel(name.as_str()) {
            Some(LinearRgba::rgb(pulse, pulse * 0.8, 0.1))
        } else if hover == Some(name.as_str()) {
            Some(LinearRgba::rgb(0.06, 0.12, 0.22))
        } else {
            None
        };
        match (glow, lit) {
            (Some(g), _) => {
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
                    m.emissive = g;
                }
            }
            (None, Some(h)) => {
                materials.remove(&mat.0);
                mat.0 = h.0.clone();
                commands.entity(e).remove::<Highlight>();
            }
            (None, None) => {}
        }
    }
}
