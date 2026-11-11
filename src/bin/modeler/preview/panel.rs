use crate::prelude::*;

/// The bottom UI line showing the pin's typed-so-far comment.
#[derive(Component)]
pub(crate) struct TypingHud;

/// The outliner's selected parts pulse in the 3D view.
pub(crate) fn highlight_selected(
    time: Res<Time>,
    outline: Res<OutlinePane>,
    parts: Query<(&Name, &MeshMaterial3d<StandardMaterial>), With<PreviewPart>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let sel: &[String] = if outline.visible {
        &outline.selected_parts
    } else {
        &[]
    };
    let pulse = 0.35 + 0.25 * (time.elapsed_secs() * 5.0).sin();
    for (name, mat) in &parts {
        let want = if sel.iter().any(|s| s == name.as_str()) {
            LinearRgba::rgb(pulse, pulse * 0.8, 0.1)
        } else {
            LinearRgba::BLACK
        };
        if let Some(m) = materials.get(&mat.0)
            && m.emissive_texture.is_none()
            && m.emissive != want
            && let Some(mut m) = materials.get_mut(&mat.0)
        {
            m.emissive = want;
        }
    }
}
