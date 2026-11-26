//! The app icon (`icons/app-icon.png`, drawn from `icons/app-icon.svg`, both
//! made by the shared icon family in `../icon-family`):
//! the Dock icon on macOS, the window icon elsewhere. Set once, on the main
//! thread, after the window exists.

use crate::prelude::*;

const PNG: &[u8] = include_bytes!("../../../../icons/app-icon.png");

/// The icon as straight RGBA (the PNG carries its own transparency).
fn rgba() -> Option<(Vec<u8>, u32, u32)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(PNG));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    buf.truncate(info.buffer_size());
    let out = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        _ => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
    };
    Some((out, w, h))
}

pub(crate) fn app_icon_plugin(app: &mut App) {
    app.add_systems(Update, set_icon.run_if(windowed));
}

/// Runs until the icon is set (the window appears a frame or two in).
fn set_icon(
    _main_thread: bevy::ecs::system::NonSendMarker,
    mut done: Local<bool>,
    mut pixels: Local<Option<(Vec<u8>, u32, u32)>>,
) {
    if *done {
        return;
    }
    // Decoded once, kept until a window takes it.
    if pixels.is_none() {
        *pixels = rgba();
        if pixels.is_none() {
            *done = true;
            return;
        }
    }
    if let Some((rgba, w, h)) = pixels.as_ref() {
        *done = apply(rgba.clone(), *w, *h);
    }
    if *done {
        *pixels = None;
    }
}

#[cfg(target_os = "macos")]
fn apply(rgba: Vec<u8>, w: u32, h: u32) -> bool {
    use objc2::AllocAnyThread;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    let Some(mtm) = MainThreadMarker::new() else {
        return true; // not the main thread: give up rather than retry forever
    };
    // NSImage reads PNG bytes: encode the masked pixels.
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let Ok(mut wr) = enc.write_header() else {
            return true;
        };
        if wr.write_image_data(&rgba).is_err() {
            return true;
        }
    }
    let data = NSData::with_bytes(&bytes);
    let Some(img) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return true;
    };
    let app = NSApplication::sharedApplication(mtm);
    unsafe { app.setApplicationIconImage(Some(&img)) };
    true
}

#[cfg(not(target_os = "macos"))]
fn apply(rgba: Vec<u8>, w: u32, h: u32) -> bool {
    let Ok(icon) = winit::window::Icon::from_rgba(rgba, w, h) else {
        return true;
    };
    bevy::winit::WINIT_WINDOWS.with_borrow(|ws| {
        let mut any = false;
        for win in ws.windows.values() {
            win.set_window_icon(Some(icon.clone()));
            any = true;
        }
        any
    })
}
