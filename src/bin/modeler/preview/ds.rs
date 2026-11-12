//! The window's design system (from `design/workbench-concept.html`): every
//! pane builds its UI from these tokens and components, so the window reads
//! as one tool.
//!
//! - **Tokens**: the colours ([`BG`], [`PANEL`], [`RAISE`], [`LINE`], [`FG`],
//!   [`DIM`], [`FAINT`], [`ACCENT`], ...), type sizes ([`Type`]), spacing
//!   ([`GAP`], [`PAD`]) and radii. [`theme`] retunes Feathers' own widgets
//!   (text inputs, the few Feathers buttons left) to the same palette.
//! - **Text**: [`text`] with a [`Type`] (body, small, caption = small
//!   uppercase heading, mono, title).
//! - **Icons**: [`Icon`], drawn once into anti-aliased white images
//!   ([`Icons`]) and tinted where used: the concept's line icons.
//! - **Interactive pieces** ([`Ui`] kinds): tabs (mode buttons, segments),
//!   rows (tree and list rows), chips (HUD), buttons and primary buttons, icon
//!   buttons. Each carries [`Ui`] (kind + on / disabled) and `Hovered`;
//!   [`paint`] restyles the background, border and its [`Ink`] children from
//!   that state, so selection and hover look the same everywhere. Clicks go
//!   through ordinary `PointerClick` observers on the entity.

// A kit: some pieces (Card, Primary, badge, ...) wait for their first user.
#![allow(dead_code)]

use bevy::feathers::constants::fonts;
use bevy::feathers::theme::ThemeProps;
use bevy::feathers::tokens::semantic;
use bevy::picking::hover::Hovered;

use crate::prelude::*;

// ---------- tokens ----------

const fn hex(rgb: u32) -> Color {
    Color::srgb(
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
    )
}

/// The window behind everything (and the graph canvas).
pub(crate) const BG: Color = hex(0x15171c);
/// Panes and the top bar.
pub(crate) const PANEL: Color = hex(0x1c1f26);
/// Raised surfaces: the selected tab, buttons, chips' fill on hover, cards.
pub(crate) const RAISE: Color = hex(0x252932);
/// Hairlines between areas and around controls.
pub(crate) const LINE: Color = hex(0x2e333e);
/// Main text.
pub(crate) const FG: Color = hex(0xe3e6ec);
/// Secondary text (labels, inactive tabs).
pub(crate) const DIM: Color = hex(0x8a91a0);
/// Tertiary text (meta, counts, hints).
pub(crate) const FAINT: Color = hex(0x5b6270);
pub(crate) const ACCENT: Color = hex(0x6fa8ff);
/// The selected row's fill.
pub(crate) const ACCENT_SOFT: Color = Color::srgba(0.435, 0.659, 1.0, 0.14);
pub(crate) const OK: Color = hex(0x5fc48a);
pub(crate) const WARN: Color = hex(0xe2b24f);
pub(crate) const BAD: Color = hex(0xec6d6d);
/// Text on an accent fill.
pub(crate) const ON_ACCENT: Color = hex(0x08162b);
/// Chips over the 3D view.
pub(crate) const CHIP_BG: Color = Color::srgba(0.082, 0.09, 0.11, 0.85);
/// Per-agent colours (pills, dots, feed), picked by name.
pub(crate) const AGENT: [Color; 5] = [
    hex(0xc792ea),
    hex(0xf6a04d),
    hex(0x4fd1c5),
    hex(0x6fa8ff),
    hex(0xec6d6d),
];

/// A stable colour for an agent (client) name.
pub(crate) fn agent_color(name: &str) -> Color {
    let h = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    AGENT[h as usize % AGENT.len()]
}

pub(crate) const GAP: f32 = 6.0;
pub(crate) const PAD: f32 = 12.0;
pub(crate) const RADIUS: f32 = 6.0;
pub(crate) const ROW_H: f32 = 26.0;

// ---------- theme ----------

/// Feathers' dark theme retuned to these tokens (its text inputs and any
/// Feathers button left match the rest).
pub(crate) fn theme() -> ThemeProps {
    let mut t = bevy::feathers::dark_theme::create_dark_theme();
    for (k, c) in [
        (semantic::SURFACE_WINDOW, BG),
        (semantic::SURFACE_PANE_BODY, PANEL),
        (semantic::SURFACE_PANE_HEADER, PANEL),
        (semantic::SURFACE_SUBPANE_BODY, PANEL),
        (semantic::SURFACE_SUBPANE_HEADER, RAISE),
        (semantic::SURFACE_GROUP, RAISE),
        (semantic::SURFACE_DIALOG, PANEL),
        (semantic::SURFACE_MENU, PANEL),
        (semantic::BORDER_DEFAULT, LINE),
        (semantic::TEXT_DEFAULT, FG),
        (semantic::TEXT_DIM, DIM),
        (semantic::TEXT_ON_ACCENT, ON_ACCENT),
        (semantic::FILL_ACCENT_DEFAULT, ACCENT),
        (semantic::FILL_ACCENT_HOVER, hex(0x8bb9ff)),
        (semantic::FILL_ACCENT_PRESSED, hex(0x9cc4ff)),
        (semantic::FILL_SOLID_DEFAULT, RAISE),
        (semantic::FILL_SOLID_HOVER, hex(0x2d323d)),
        (semantic::FILL_SOLID_PRESSED, hex(0x353b47)),
        (semantic::FILL_ITEM_HOVER, RAISE),
        (semantic::FILL_ITEM_PRESSED, hex(0x2d323d)),
        (semantic::FILL_FIELD_DEFAULT, BG),
        (semantic::FILL_FIELD_HOVER, BG),
        (semantic::FILL_FIELD_PRESSED, BG),
        (semantic::FOCUS_RING, ACCENT.with_alpha(0.5)),
    ] {
        t.semantic_base.insert(k, c);
    }
    t.semantic_overrides.clear();
    t
}

// ---------- text ----------

/// The type scale.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Type {
    /// 13 px: rows, buttons, most text.
    Body,
    /// 12 px: values, comments, secondary lines.
    Small,
    /// 11 px uppercase, dim: section headings ("LAYERS").
    Caption,
    /// 11 px: meta at the end of a row, counts, timestamps.
    Meta,
    /// 12 px monospace: values, node types, tool names.
    Mono,
    /// 13 px bold: the document name, a pane's subject.
    Title,
}

/// A text bundle in the scale's font, size and default colour.
pub(crate) fn text(assets: &AssetServer, ty: Type, s: impl Into<String>) -> impl Bundle {
    styled(assets, ty, s, None, false)
}

/// [`text`] in another colour.
pub(crate) fn text_in(
    assets: &AssetServer,
    ty: Type,
    s: impl Into<String>,
    color: Color,
) -> impl Bundle {
    styled(assets, ty, s, Some(color), false)
}

/// Wrapping text (comments, hints).
pub(crate) fn text_wrap(assets: &AssetServer, ty: Type, s: impl Into<String>) -> impl Bundle {
    styled(assets, ty, s, None, true)
}

fn styled(
    assets: &AssetServer,
    ty: Type,
    s: impl Into<String>,
    color: Option<Color>,
    wrap: bool,
) -> impl Bundle {
    let s: String = s.into();
    let (font, px, base, s) = match ty {
        Type::Body => (fonts::REGULAR, 13.0, FG, s),
        Type::Small => (fonts::REGULAR, 12.0, FG, s),
        Type::Caption => (fonts::REGULAR, 11.0, FAINT, s.to_uppercase()),
        Type::Meta => (fonts::REGULAR, 11.0, FAINT, s),
        Type::Mono => (fonts::MONO, 11.5, DIM, s),
        Type::Title => (fonts::BOLD, 13.0, FG, s),
    };
    (
        Text::new(s),
        TextFont {
            font: assets.load(font).into(),
            font_size: FontSize::Px(px),
            ..default()
        },
        TextColor(color.unwrap_or(base)),
        if wrap {
            TextLayout::default()
        } else {
            TextLayout::no_wrap()
        },
    )
}

// ---------- icons ----------

/// The concept's line icons (16 x 16, 1.6 px strokes).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Icon {
    Grid,
    Chevron,
    ChevronRight,
    Cube,
    House,
    Pin,
    Mount,
    Zone,
    Route,
    Block,
    Part,
    Eye,
    Hide,
    Bone,
    Play,
    Pause,
    X,
    View,
    Light,
    Swatch,
    Search,
    Comment,
    Layers,
    Plus,
}

/// Stroke primitives in the 16-unit icon box.
enum Prim {
    Line(&'static [(f32, f32)]),
    Closed(&'static [(f32, f32)]),
    Circle((f32, f32), f32),
    Disc((f32, f32), f32),
    Fill(&'static [(f32, f32)]),
}

fn shapes(i: Icon) -> Vec<Prim> {
    use Prim::*;
    match i {
        Icon::Grid => vec![
            Closed(&[(2.0, 2.0), (7.0, 2.0), (7.0, 7.0), (2.0, 7.0)]),
            Closed(&[(9.0, 2.0), (14.0, 2.0), (14.0, 7.0), (9.0, 7.0)]),
            Closed(&[(2.0, 9.0), (7.0, 9.0), (7.0, 14.0), (2.0, 14.0)]),
            Closed(&[(9.0, 9.0), (14.0, 9.0), (14.0, 14.0), (9.0, 14.0)]),
        ],
        Icon::Chevron => vec![Line(&[(4.0, 6.0), (8.0, 10.0), (12.0, 6.0)])],
        Icon::ChevronRight => vec![Line(&[(6.0, 4.0), (10.0, 8.0), (6.0, 12.0)])],
        Icon::Cube => vec![
            Closed(&[
                (8.0, 1.8),
                (13.5, 4.8),
                (13.5, 11.2),
                (8.0, 14.2),
                (2.5, 11.2),
                (2.5, 4.8),
            ]),
            Line(&[(2.5, 4.8), (8.0, 7.8), (13.5, 4.8)]),
            Line(&[(8.0, 7.8), (8.0, 14.2)]),
        ],
        Icon::House => vec![
            Closed(&[
                (2.5, 7.5),
                (8.0, 3.0),
                (13.5, 7.5),
                (13.5, 13.0),
                (2.5, 13.0),
            ]),
            Line(&[(6.5, 13.0), (6.5, 10.0), (9.5, 10.0), (9.5, 13.0)]),
        ],
        Icon::Pin => vec![
            Closed(&[
                (8.0, 14.0),
                (5.0, 10.5),
                (3.6, 7.6),
                (3.8, 5.0),
                (5.2, 3.0),
                (8.0, 2.0),
                (10.8, 3.0),
                (12.2, 5.0),
                (12.4, 7.6),
                (11.0, 10.5),
            ]),
            Circle((8.0, 6.5), 1.5),
        ],
        Icon::Mount => vec![Closed(&[
            (1.5, 13.0),
            (6.0, 6.0),
            (9.0, 10.5),
            (11.0, 7.5),
            (14.5, 13.0),
        ])],
        Icon::Zone => vec![
            Line(&[(2.5, 5.5), (2.5, 3.5), (4.5, 3.5)]),
            Line(&[(11.5, 3.5), (13.5, 3.5), (13.5, 5.5)]),
            Line(&[(13.5, 10.5), (13.5, 12.5), (11.5, 12.5)]),
            Line(&[(4.5, 12.5), (2.5, 12.5), (2.5, 10.5)]),
            Line(&[(6.5, 3.5), (9.5, 3.5)]),
            Line(&[(6.5, 12.5), (9.5, 12.5)]),
        ],
        Icon::Route => vec![
            Circle((3.5, 12.5), 1.5),
            Circle((12.5, 3.5), 1.5),
            Line(&[
                (5.0, 12.5),
                (8.5, 12.5),
                (10.0, 11.7),
                (10.5, 10.5),
                (10.0, 9.3),
                (8.5, 8.5),
                (7.5, 8.5),
                (6.0, 7.7),
                (5.5, 6.5),
                (6.0, 5.3),
                (7.5, 4.5),
                (11.0, 4.5),
            ]),
        ],
        Icon::Block => vec![
            Closed(&[(3.0, 3.0), (13.0, 3.0), (13.0, 13.0), (3.0, 13.0)]),
            Line(&[(3.0, 8.0), (13.0, 8.0)]),
            Line(&[(8.0, 3.0), (8.0, 13.0)]),
        ],
        Icon::Part => vec![Closed(&[
            (3.0, 11.5),
            (8.0, 14.0),
            (13.0, 11.5),
            (13.0, 4.5),
            (8.0, 2.0),
            (3.0, 4.5),
        ])],
        Icon::Eye => vec![
            Closed(&[
                (1.5, 8.0),
                (3.5, 5.4),
                (5.8, 3.9),
                (8.0, 3.5),
                (10.2, 3.9),
                (12.5, 5.4),
                (14.5, 8.0),
                (12.5, 10.6),
                (10.2, 12.1),
                (8.0, 12.5),
                (5.8, 12.1),
                (3.5, 10.6),
            ]),
            Circle((8.0, 8.0), 2.0),
        ],
        Icon::Hide => vec![
            Closed(&[
                (2.0, 8.0),
                (3.8, 5.6),
                (5.9, 4.3),
                (8.0, 4.0),
                (10.1, 4.3),
                (12.2, 5.6),
                (14.0, 8.0),
                (12.2, 10.4),
                (10.1, 11.7),
                (8.0, 12.0),
                (5.9, 11.7),
                (3.8, 10.4),
            ]),
            Line(&[(3.0, 13.0), (13.0, 3.0)]),
        ],
        Icon::Bone => vec![
            Circle((3.5, 12.5), 1.6),
            Circle((12.5, 3.5), 1.6),
            Line(&[(4.7, 11.3), (11.3, 4.7)]),
        ],
        Icon::Play => vec![Fill(&[(5.0, 3.5), (12.0, 8.0), (5.0, 12.5)])],
        Icon::Pause => vec![
            Fill(&[(4.5, 3.5), (6.8, 3.5), (6.8, 12.5), (4.5, 12.5)]),
            Fill(&[(9.2, 3.5), (11.5, 3.5), (11.5, 12.5), (9.2, 12.5)]),
        ],
        Icon::X => vec![
            Line(&[(4.0, 4.0), (12.0, 12.0)]),
            Line(&[(12.0, 4.0), (4.0, 12.0)]),
        ],
        Icon::View => vec![
            Circle((8.0, 8.0), 5.5),
            Fill(&[
                (8.0, 2.5),
                (10.1, 2.9),
                (11.9, 4.1),
                (13.1, 5.9),
                (13.5, 8.0),
                (13.1, 10.1),
                (11.9, 11.9),
                (10.1, 13.1),
                (8.0, 13.5),
            ]),
        ],
        Icon::Light => vec![
            Circle((8.0, 7.0), 3.5),
            Line(&[(6.5, 12.0), (9.5, 12.0)]),
            Line(&[(7.0, 14.0), (9.0, 14.0)]),
        ],
        Icon::Swatch => vec![Fill(&[(3.0, 3.0), (13.0, 3.0), (13.0, 13.0), (3.0, 13.0)])],
        Icon::Search => vec![Circle((7.0, 7.0), 4.5), Line(&[(10.5, 10.5), (14.0, 14.0)])],
        Icon::Comment => vec![Closed(&[
            (2.5, 3.0),
            (13.5, 3.0),
            (13.5, 11.0),
            (7.0, 11.0),
            (4.0, 13.5),
            (4.0, 11.0),
            (2.5, 11.0),
        ])],
        Icon::Layers => vec![
            Closed(&[(8.0, 2.5), (14.0, 5.5), (8.0, 8.5), (2.0, 5.5)]),
            Line(&[(2.0, 8.5), (8.0, 11.5), (14.0, 8.5)]),
            Line(&[(2.0, 11.0), (8.0, 14.0), (14.0, 11.0)]),
        ],
        Icon::Plus => vec![
            Line(&[(8.0, 3.0), (8.0, 13.0)]),
            Line(&[(3.0, 8.0), (13.0, 8.0)]),
        ],
    }
}

/// Distance from `p` to segment `a`-`b`.
fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, y) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
    (x * x + y * y).sqrt()
}

fn inside(p: (f32, f32), poly: &[(f32, f32)]) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

/// Coverage (0..1) of the icon at a point, from its distance field.
fn coverage(prims: &[Prim], p: (f32, f32), half_stroke: f32, aa: f32) -> f32 {
    let mut d = f32::MAX;
    let edges = |pts: &[(f32, f32)], closed: bool, d: &mut f32| {
        let n = pts.len();
        let m = if closed { n } else { n.saturating_sub(1) };
        for i in 0..m {
            *d = d.min(seg_dist(p, pts[i], pts[(i + 1) % n]) - half_stroke);
        }
    };
    for prim in prims {
        match prim {
            Prim::Line(pts) => edges(pts, false, &mut d),
            Prim::Closed(pts) => edges(pts, true, &mut d),
            Prim::Circle(c, r) => {
                let l = ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt();
                d = d.min((l - r).abs() - half_stroke);
            }
            Prim::Disc(c, r) => {
                let l = ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt();
                d = d.min(l - r);
            }
            Prim::Fill(pts) => {
                if inside(p, pts) {
                    d = d.min(-1.0);
                } else {
                    edges(pts, true, &mut d);
                }
            }
        }
    }
    (0.5 - d / aa).clamp(0.0, 1.0)
}

/// Every icon drawn once (white, alpha = coverage) at 3x for crisp scaling.
#[derive(Resource, Default)]
pub(crate) struct Icons(std::collections::HashMap<Icon, Handle<Image>>);

impl Icons {
    pub(crate) fn get(&mut self, images: &mut Assets<Image>, icon: Icon) -> Handle<Image> {
        self.0
            .entry(icon)
            .or_insert_with(|| images.add(draw_icon(icon, 48)))
            .clone()
    }
}

fn draw_icon(icon: Icon, px: u32) -> Image {
    let prims = shapes(icon);
    let s = 16.0 / px as f32;
    let mut data = Vec::with_capacity((px * px * 4) as usize);
    for y in 0..px {
        for x in 0..px {
            let p = ((x as f32 + 0.5) * s, (y as f32 + 0.5) * s);
            let a = coverage(&prims, p, 0.8, s);
            data.extend_from_slice(&[255, 255, 255, (a * 255.0).round() as u8]);
        }
    }
    let mut img = Image::new(
        bevy::render::render_resource::Extent3d {
            width: px,
            height: px,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = bevy::image::ImageSampler::linear();
    img
}

/// An icon node, `size` logical px, tinted (add an [`Ink`] for it to
/// follow its piece's state).
pub(crate) fn icon(
    icons: &mut Icons,
    images: &mut Assets<Image>,
    i: Icon,
    size: f32,
    tint: Color,
) -> impl Bundle {
    (
        ImageNode::new(icons.get(images, i)).with_color(tint),
        Node {
            width: Val::Px(size),
            height: Val::Px(size),
            flex_shrink: 0.0,
            ..default()
        },
    )
}

// ---------- interactive pieces ----------

/// What a piece is, which sets how [`paint`] draws its states.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// Mode buttons and segment items: dim text, RAISE when on.
    Tab,
    /// Tree / list rows: RAISE on hover, ACCENT_SOFT when on.
    Row,
    /// HUD chips over the 3D view: translucent, bordered.
    Chip,
    /// Ordinary buttons: RAISE, lighter on hover.
    Button,
    /// The one main action: ACCENT with dark text.
    Primary,
    /// Icon-only: no fill, RAISE on hover.
    IconButton,
    /// A card (grid tile, graph node): PANEL, border LINE, accent when on.
    Card,
}

/// State of an interactive piece; change `on` / `disabled` and [`paint`]
/// restyles it (and its [`Ink`] descendants).
#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub(crate) struct Ui {
    pub(crate) kind: Kind,
    pub(crate) on: bool,
    pub(crate) disabled: bool,
}

impl Ui {
    pub(crate) fn new(kind: Kind) -> Self {
        Ui {
            kind,
            on: false,
            disabled: false,
        }
    }
    pub(crate) fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }
}

/// A piece's text or icon whose colour follows the piece's state:
/// `Main` (labels), `Meta` (counts, kbd hints), `Icon`, or `Fixed` (leave it).
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Ink {
    Main,
    Meta,
    Icon,
    Fixed,
}

/// The node for a piece of `kind` (padding, radius, row direction).
pub(crate) fn piece(kind: Kind) -> impl Bundle {
    let (pad, radius, border) = match kind {
        Kind::Tab => (UiRect::axes(Val::Px(12.0), Val::Px(6.0)), RADIUS, 0.0),
        Kind::Row => (UiRect::axes(Val::Px(6.0), Val::Px(0.0)), 5.0, 0.0),
        Kind::Chip => (UiRect::axes(Val::Px(8.0), Val::Px(4.0)), RADIUS, 1.0),
        Kind::Button => (UiRect::axes(Val::Px(10.0), Val::Px(6.0)), RADIUS, 1.0),
        Kind::Primary => (UiRect::axes(Val::Px(10.0), Val::Px(6.0)), RADIUS, 0.0),
        Kind::IconButton => (UiRect::all(Val::Px(4.0)), 5.0, 0.0),
        Kind::Card => (UiRect::all(Val::Px(0.0)), 10.0, 1.0),
    };
    (
        Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(GAP + 2.0),
            padding: pad,
            border: UiRect::all(Val::Px(border)),
            border_radius: BorderRadius::all(Val::Px(radius)),
            min_height: if kind == Kind::Row {
                Val::Px(ROW_H)
            } else {
                Val::Auto
            },
            ..default()
        },
        Ui::new(kind),
        Hovered::default(),
        BackgroundColor(Color::NONE),
        BorderColor::all(Color::NONE),
        Pickable::default(),
    )
}

/// Colours for a piece in a state: (fill, border, main ink, meta ink).
fn look(ui: Ui, hover: bool) -> (Color, Color, Color, Color) {
    let none = Color::NONE;
    if ui.disabled {
        return match ui.kind {
            Kind::Chip => (CHIP_BG, LINE, FAINT, FAINT),
            _ => (none, none, FAINT.with_alpha(0.6), FAINT.with_alpha(0.6)),
        };
    }
    match (ui.kind, ui.on, hover) {
        (Kind::Tab, true, _) => (RAISE, none, FG, DIM),
        (Kind::Tab, false, true) => (none, none, FG, FAINT),
        (Kind::Tab, false, false) => (none, none, DIM, FAINT),
        (Kind::Row, true, _) => (ACCENT_SOFT, none, FG, DIM),
        (Kind::Row, false, true) => (RAISE, none, FG, FAINT),
        (Kind::Row, false, false) => (none, none, FG, FAINT),
        (Kind::Chip, on, h) => (
            if h { RAISE.with_alpha(0.92) } else { CHIP_BG },
            if on { ACCENT } else { LINE },
            if on || h { FG } else { DIM },
            DIM,
        ),
        (Kind::Button, _, h) => (if h { hex(0x2d323d) } else { RAISE }, LINE, FG, DIM),
        (Kind::Primary, _, h) => (
            if h { hex(0x8bb9ff) } else { ACCENT },
            none,
            ON_ACCENT,
            ON_ACCENT,
        ),
        (Kind::IconButton, on, h) => (
            if h || on { RAISE } else { none },
            none,
            if h || on { FG } else { DIM },
            DIM,
        ),
        (Kind::Card, on, h) => (
            PANEL,
            if on {
                ACCENT
            } else if h {
                FAINT
            } else {
                LINE
            },
            FG,
            FAINT,
        ),
    }
}

/// Restyles pieces whose state or hover changed, and their inks.
#[allow(clippy::type_complexity)]
pub(crate) fn paint(
    pieces: Query<
        (
            Entity,
            &Ui,
            &Hovered,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        Or<(Changed<Ui>, Changed<Hovered>, Added<Ui>)>,
    >,
    children: Query<&Children>,
    stops: Query<(), With<Ui>>,
    mut inks: Query<(&Ink, Option<&mut TextColor>, Option<&mut ImageNode>)>,
) {
    for (e, ui, hovered, mut bg, mut border) in pieces {
        let (fill, line, main, meta) = look(*ui, hovered.get());
        bg.set_if_neq(BackgroundColor(fill));
        border.set_if_neq(BorderColor::all(line));
        // Inks below this piece, not inside nested pieces.
        let mut stack: Vec<Entity> = children.get(e).map(|c| c.to_vec()).unwrap_or_default();
        while let Some(c) = stack.pop() {
            if stops.contains(c) {
                continue;
            }
            if let Ok((ink, tc, img)) = inks.get_mut(c) {
                let col = match ink {
                    Ink::Main | Ink::Icon => main,
                    Ink::Meta => meta,
                    Ink::Fixed => continue,
                };
                if let Some(mut t) = tc {
                    t.set_if_neq(TextColor(col));
                }
                if let Some(mut i) = img
                    && i.color != col
                {
                    i.color = col;
                }
            }
            if let Ok(cs) = children.get(c) {
                stack.extend(cs.iter());
            }
        }
    }
}

// ---------- small static pieces ----------

/// A section heading ("LAYERS") with optional meta at the right.
pub(crate) fn section(
    c: &mut Commands,
    assets: &AssetServer,
    parent: Entity,
    title: &str,
    meta: Option<String>,
) -> Entity {
    let row = c
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                padding: UiRect::new(Val::Px(PAD), Val::Px(PAD), Val::Px(12.0), Val::Px(6.0)),
                flex_shrink: 0.0,
                ..default()
            },
            ChildOf(parent),
        ))
        .id();
    c.spawn((text(assets, Type::Caption, title), ChildOf(row)));
    if let Some(m) = meta {
        c.spawn((text(assets, Type::Meta, m), ChildOf(row)));
    }
    row
}

/// A small round dot (agents, status).
pub(crate) fn dot(color: Color, size: f32) -> impl Bundle {
    (
        Node {
            width: Val::Px(size),
            height: Val::Px(size),
            flex_shrink: 0.0,
            border_radius: BorderRadius::all(Val::Px(size / 2.0)),
            ..default()
        },
        BackgroundColor(color),
    )
}

/// A colour swatch (materials).
pub(crate) fn swatch(color: Color) -> impl Bundle {
    (
        Node {
            width: Val::Px(14.0),
            height: Val::Px(14.0),
            flex_shrink: 0.0,
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(color),
    )
}

/// A pill badge with a count or a short word.
pub(crate) fn badge(
    c: &mut Commands,
    assets: &AssetServer,
    parent: Entity,
    s: impl Into<String>,
) -> Entity {
    let b = c
        .spawn((
            Node {
                padding: UiRect::axes(Val::Px(6.0), Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(99.0)),
                flex_shrink: 0.0,
                ..default()
            },
            BackgroundColor(RAISE),
            ChildOf(parent),
        ))
        .id();
    c.spawn((text(assets, Type::Meta, s), Ink::Meta, ChildOf(b)));
    b
}

/// A key hint ("1").
pub(crate) fn kbd(c: &mut Commands, assets: &AssetServer, parent: Entity, key: &str) -> Entity {
    let k = c
        .spawn((
            Node {
                padding: UiRect::axes(Val::Px(4.0), Val::Px(0.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(3.0)),
                ..default()
            },
            BorderColor::all(LINE),
            ChildOf(parent),
        ))
        .id();
    c.spawn((text(assets, Type::Meta, key), Ink::Meta, ChildOf(k)));
    k
}

/// A label / value row (properties).
pub(crate) fn kv(
    c: &mut Commands,
    assets: &AssetServer,
    parent: Entity,
    k: &str,
    v: &str,
) -> Entity {
    let row = c
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                padding: UiRect::vertical(Val::Px(2.0)),
                ..default()
            },
            ChildOf(parent),
        ))
        .id();
    c.spawn((
        text_in(assets, Type::Small, k, DIM),
        Node {
            width: Val::Px(90.0),
            flex_shrink: 0.0,
            ..default()
        },
        ChildOf(row),
    ));
    c.spawn((
        styled(assets, Type::Mono, v, Some(FG), true),
        Node {
            flex_shrink: 1.0,
            ..default()
        },
        ChildOf(row),
    ));
    row
}

/// A 1 px divider.
pub(crate) fn divider(vertical: bool) -> impl Bundle {
    (
        Node {
            width: if vertical {
                Val::Px(1.0)
            } else {
                Val::Percent(100.0)
            },
            height: if vertical {
                Val::Percent(100.0)
            } else {
                Val::Px(1.0)
            },
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(LINE),
    )
}

pub(crate) fn ds_plugin(app: &mut App) {
    app.init_resource::<Icons>()
        .add_systems(PostUpdate, paint.run_if(windowed));
}
