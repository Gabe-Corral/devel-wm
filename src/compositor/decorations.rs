//! Per-window decoration geometry and render elements, interleaved with the
//! corresponding client surface in Space so overlapping windows stack correctly.

use std::{
    collections::HashMap,
    fs,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

use ab_glyph::{Font, FontArc, FontVec, ScaleFont, point};
use smithay::{
    backend::renderer::{
        ImportAll, ImportMem, Renderer,
        element::{
            AsRenderElements, Kind,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            utils::CropRenderElement,
        },
    },
    desktop::{Window, space::SpaceElement},
    output::Output,
    utils::{IsAlive, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use smithay::backend::allocator::Fourcc;

use crate::config::{DecorationColors, HexColor, TitleFontSettings};

pub const TITLE_HEIGHT: i32 = 24;
pub const BORDER: i32 = 2;

// Fourcc::Argb8888 is BGRA in native little-endian memory, not RGBA.
fn argb8888_bytes(color: HexColor) -> [u8; 4] {
    let [red, green, blue] = color.0;
    [blue, green, red, 255]
}

pub fn insets(decorated: bool) -> (i32, i32) {
    if decorated {
        (BORDER, TITLE_HEIGHT)
    } else {
        (0, 0)
    }
}

fn frame_size_for_client(client: Size<i32, Logical>, decorated: bool) -> Size<i32, Logical> {
    let (side, title_height) = insets(decorated);
    (client.w + side * 2, client.h + title_height + side).into()
}

/// A continuous frame around the titlebar and content. The title's opaque
/// background covers the full top row, so draw these edges in front of it.
fn border_rects(width: i32, height: i32) -> [(i32, i32, i32, i32); 4] {
    [
        (0, 0, BORDER, height),
        (width - BORDER, 0, BORDER, height),
        (BORDER, 0, width - BORDER * 2, BORDER),
        (BORDER, height - BORDER, width - BORDER * 2, BORDER),
    ]
}

#[derive(Clone, Debug)]
pub(crate) struct TitleFont {
    face: FontArc,
    size: u16,
}

impl TitleFont {
    pub fn load(settings: &TitleFontSettings) -> Self {
        let face = settings
            .family
            .as_deref()
            .and_then(|family| {
                let font = installed_font(family.trim());
                if font.is_none() {
                    tracing::warn!(family, "title font unavailable; using bundled DejaVu Sans");
                }
                font
            })
            .unwrap_or_else(bundled_font);
        Self {
            face,
            size: settings.size,
        }
    }
}

impl Default for TitleFont {
    fn default() -> Self {
        Self::load(&TitleFontSettings::default())
    }
}

fn bundled_font() -> FontArc {
    static FONT: OnceLock<FontArc> = OnceLock::new();
    FONT.get_or_init(|| {
        FontArc::try_from_slice(include_bytes!("../../assets/fonts/DejaVuSans.ttf"))
            .expect("bundled title font")
    })
    .clone()
}

fn matched_font<'a>(text: &'a str, requested: &str) -> Option<(&'a str, u32)> {
    let mut lines = text.lines();
    let family = lines.next()?;
    let generic = ["sans-serif", "serif", "monospace", "system-ui"]
        .iter()
        .any(|alias| requested.eq_ignore_ascii_case(alias));
    if !generic
        && !family
            .split(',')
            .any(|name| name.trim().eq_ignore_ascii_case(requested))
    {
        return None;
    }
    let path = lines.next()?;
    let index = lines.next()?.parse().ok()?;
    (!path.is_empty()).then_some((path, index))
}

fn installed_font(family: &str) -> Option<FontArc> {
    // fontconfig resolves family names once at startup; missing fc-match,
    // substituted families and unsupported font files fall back to our TTF.
    let output = Command::new("fc-match")
        .args(["-f", "%{family}\n%{file}\n%{index}\n", "--", family])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let (path, index) = matched_font(&text, family)?;
    FontVec::try_from_vec_and_index(fs::read(path).ok()?, index)
        .ok()
        .map(FontArc::new)
}

fn paint_title(
    pixels: &mut [u8],
    logical_width: i32,
    title: &str,
    focused: bool,
    scale: i32,
    colors: DecorationColors,
    font: &TitleFont,
) {
    paint_title_pixels(
        pixels,
        logical_width * scale,
        title,
        focused,
        scale,
        colors,
        font,
    );
}

fn paint_title_pixels(
    pixels: &mut [u8],
    width: i32,
    title: &str,
    focused: bool,
    scale: i32,
    colors: DecorationColors,
    font: &TitleFont,
) {
    let height = TITLE_HEIGHT * scale;
    let background = if focused {
        argb8888_bytes(colors.focused_titlebar)
    } else {
        argb8888_bytes(colors.unfocused_titlebar)
    };
    let text = argb8888_bytes(colors.title_text);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&background);
    }
    let face = &font.face;
    let font_size = f32::from(font.size) * scale as f32;
    let scaled = face.as_scaled(font_size);
    let baseline = (height as f32 - (scaled.ascent() - scaled.descent())) / 2.0 + scaled.ascent();
    let padding = 8.0 * scale as f32;
    let mut x = padding;
    let mut previous = None;
    for ch in title.chars() {
        let id = face.glyph_id(ch);
        let id = if id.0 == 0 { face.glyph_id('?') } else { id };
        let kern = previous.map_or(0.0, |last| scaled.kern(last, id));
        let next_x = x + kern;
        let advance = scaled.h_advance(id);
        if next_x + advance > width as f32 - padding {
            break;
        }
        x = next_x;
        if let Some(glyph) =
            face.outline_glyph(id.with_scale_and_position(font_size, point(x, baseline)))
        {
            let bounds = glyph.px_bounds();
            glyph.draw(|dx, dy, coverage| {
                let px = bounds.min.x as i32 + dx as i32;
                let py = bounds.min.y as i32 + dy as i32;
                if px >= padding as i32 && px < width - padding as i32 && py >= 0 && py < height {
                    let offset = ((py * width + px) * 4) as usize;
                    for channel in 0..3 {
                        pixels[offset + channel] = (background[channel] as f32 * (1.0 - coverage)
                            + f32::from(text[channel]) * coverage)
                            .round() as u8;
                    }
                }
            });
        }
        x += advance;
        previous = Some(id);
    }
}

fn paint_group_title(
    pixels: &mut [u8],
    logical_width: i32,
    tabs: &[(String, bool)],
    scale: i32,
    colors: DecorationColors,
    font: &TitleFont,
) {
    let width = (logical_width * scale) as usize;
    let count = tabs.len();
    for (index, (title, focused)) in tabs.iter().enumerate() {
        let left = width * index / count;
        let right = width * (index + 1) / count;
        let tab_width = right - left;
        let mut tab = vec![0; tab_width * (TITLE_HEIGHT * scale) as usize * 4];
        // Divide at physical pixels so the tabs are equal to within one pixel
        // at every integer output scale.
        paint_title_pixels(
            &mut tab,
            tab_width as i32,
            title,
            *focused,
            scale,
            colors,
            font,
        );
        for row in 0..(TITLE_HEIGHT * scale) as usize {
            let destination = (row * width + left) * 4;
            let source = row * tab_width * 4;
            pixels[destination..destination + tab_width * 4]
                .copy_from_slice(&tab[source..source + tab_width * 4]);
            if index > 0 && left > 0 {
                let separator = (row * width + left) * 4;
                pixels[separator..separator + 4]
                    .copy_from_slice(&argb8888_bytes(colors.focused_border));
            }
        }
    }
}

struct Decoration {
    colors: DecorationColors,
    font: TitleFont,
    enabled: bool,
    floating: bool,
    title: String,
    tabs: Vec<(String, bool)>,
    focused: bool,
    width: i32,
    height: i32,
    owner_clip: Option<(Rectangle<i32, Logical>, Point<i32, Logical>)>,
    title_buffers: HashMap<i32, MemoryRenderBuffer>,
    borders: [SolidColorBuffer; 4],
}

impl Decoration {
    fn new(colors: DecorationColors, font: TitleFont) -> Self {
        Self {
            colors,
            font,
            enabled: false,
            floating: false,
            title: String::new(),
            tabs: Vec::new(),
            focused: false,
            width: 1,
            height: 1,
            owner_clip: None,
            title_buffers: HashMap::new(),
            borders: std::array::from_fn(|_| {
                SolidColorBuffer::new((1, 1), colors.unfocused_border.solid())
            }),
        }
    }

    fn update(
        &mut self,
        enabled: bool,
        title: String,
        focused: bool,
        width: i32,
        height: i32,
    ) -> bool {
        let width = width.max(1);
        let height = height.max(1);
        if (
            self.enabled,
            &self.title,
            self.focused,
            self.width,
            self.height,
        ) == (enabled, &title, focused, width, height)
        {
            return false;
        }
        self.enabled = enabled;
        self.title = title;
        self.focused = focused;
        self.width = width;
        self.height = height;
        if enabled {
            self.title_buffers.clear();
            self.update_borders();
        }
        true
    }

    fn update_borders(&mut self) {
        let normal = if self.focused {
            self.colors.focused_border.solid()
        } else {
            self.colors.unfocused_border.solid()
        };
        for (border, (_, _, w, h)) in self
            .borders
            .iter_mut()
            .zip(border_rects(self.width, self.height))
        {
            border.update((w.max(1), h.max(1)), normal);
        }
    }

    fn set_tabs(&mut self, tabs: Vec<(String, bool)>) -> bool {
        if self.tabs == tabs {
            return false;
        }
        self.tabs = tabs;
        self.title_buffers.clear();
        true
    }

    fn title_buffer(&mut self, scale: i32) -> &MemoryRenderBuffer {
        self.title_buffers.entry(scale).or_insert_with(|| {
            let size = (self.width * scale, TITLE_HEIGHT * scale);
            let mut buffer =
                MemoryRenderBuffer::new(Fourcc::Argb8888, size, scale, Transform::Normal, None);
            let mut context = buffer.render();
            context
                .draw(|pixels| {
                    if self.tabs.len() > 1 {
                        paint_group_title(
                            pixels,
                            self.width,
                            &self.tabs,
                            scale,
                            self.colors,
                            &self.font,
                        );
                    } else {
                        paint_title(
                            pixels,
                            self.width,
                            &self.title,
                            self.focused,
                            scale,
                            self.colors,
                            &self.font,
                        );
                    }
                    Ok::<_, ()>(vec![Rectangle::from_size(size.into())])
                })
                .expect("infallible title draw");
            context.update_opaque_regions(Some(vec![Rectangle::from_size(size.into())]));
            drop(context);
            buffer
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_frame_and_borders_use_committed_client_size_not_requested_size() {
        let committed: Size<i32, Logical> = (300, 180).into();
        let requested: Size<i32, Logical> = (420, 240).into();
        let frame = frame_size_for_client(committed, true);
        assert_eq!((frame.w, frame.h), (304, 206));
        assert_ne!(frame, frame_size_for_client(requested, true));
        let [left, right, top, bottom] = border_rects(frame.w, frame.h);
        assert_eq!(left, (0, 0, BORDER, frame.h));
        assert_eq!(right.0 + right.2, frame.w);
        assert_eq!(top.2 + 2 * BORDER, frame.w);
        assert_eq!(bottom.1 + bottom.3, frame.h);
        assert_eq!(frame_size_for_client(committed, false), committed);
    }

    #[test]
    fn fontconfig_match_rejects_unavailable_families_and_keeps_face_index() {
        assert_eq!(
            matched_font("Iosevka\n/fonts/iosevka.ttc\n2\n", "Iosevka"),
            Some(("/fonts/iosevka.ttc", 2))
        );
        assert_eq!(
            matched_font("DejaVu Sans\n/fonts/dejavu.ttf\n0\n", "Missing Font"),
            None
        );
        assert_eq!(
            matched_font("DejaVu Sans\n/fonts/dejavu.ttf\n0\n", "sans-serif"),
            Some(("/fonts/dejavu.ttf", 0))
        );
        assert_eq!(
            matched_font("Iosevka\n/fonts/iosevka.ttc\nbad\n", "Iosevka"),
            None
        );
    }

    #[test]
    fn configured_font_size_affects_single_and_grouped_titles_at_both_scales() {
        let colors = DecorationColors::default();
        let small = TitleFont::load(&TitleFontSettings {
            family: None,
            size: 8,
        });
        let large = TitleFont::load(&TitleFontSettings {
            family: None,
            size: 18,
        });
        for scale in [1, 2] {
            let mut first = vec![0; (120 * scale * TITLE_HEIGHT * scale * 4) as usize];
            let mut second = first.clone();
            paint_title(&mut first, 120, "Test", true, scale, colors, &small);
            paint_title(&mut second, 120, "Test", true, scale, colors, &large);
            assert_ne!(first, second);
            let tabs = [("Test".to_owned(), true), ("Next".to_owned(), false)];
            paint_group_title(&mut first, 120, &tabs, scale, colors, &small);
            paint_group_title(&mut second, 120, &tabs, scale, colors, &large);
            assert_ne!(first, second);
        }
    }

    #[test]
    fn title_pixels_are_clipped_and_change_with_focus() {
        let colors = DecorationColors::default();
        let font = TitleFont::default();
        let mut pixels = vec![0; 23 * TITLE_HEIGHT as usize * 4];
        paint_title(&mut pixels, 23, "overlong title", false, 1, colors, &font);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == argb8888_bytes(colors.unfocused_titlebar))
        );
        let mut pixels = vec![0; 32 * TITLE_HEIGHT as usize * 4];
        paint_title(&mut pixels, 32, "A never fits", true, 1, colors, &font);
        assert_eq!(&pixels[0..4], &argb8888_bytes(colors.focused_titlebar));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[2] > colors.focused_titlebar.0[0] && pixel[3] == 0xff)
        );
        assert_eq!(insets(true), (BORDER, TITLE_HEIGHT));
        assert_eq!(insets(false), (0, 0));
    }

    #[test]
    fn title_raster_is_drawn_at_physical_scale_without_resampling() {
        let colors = DecorationColors::default();
        let font = TitleFont::default();
        let mut pixels = vec![0; 64 * (TITLE_HEIGHT * 2) as usize * 4];
        paint_title(&mut pixels, 32, "A", true, 2, colors, &font);
        assert!(pixels.as_chunks::<4>().0.iter().any(|pixel| {
            pixel[2] > colors.focused_titlebar.0[0]
                && pixel[2] < colors.title_text.0[0]
                && pixel[3] == 0xff
        }));
        let mut decoration = Decoration::new(colors, font);
        decoration.update(true, "A".into(), true, 32, 48);
        decoration.title_buffer(2);
        decoration.title_buffer(1);
        assert!(decoration.title_buffers.contains_key(&2));
        assert!(decoration.title_buffers.contains_key(&1));
        decoration.update(true, "B".into(), true, 32, 48);
        assert!(decoration.title_buffers.is_empty());
    }

    #[test]
    fn grouped_titlebar_splits_evenly_at_both_scales_and_selects_one_tab() {
        let colors = DecorationColors::default();
        let font = TitleFont::default();
        for scale in [1, 2] {
            let width = 101 * scale;
            let mut pixels = vec![0; (width * TITLE_HEIGHT * scale * 4) as usize];
            let tabs = vec![("".into(), false), ("".into(), true), ("".into(), false)];
            paint_group_title(&mut pixels, 101, &tabs, scale, colors, &font);
            let row = 4 * width as usize;
            let pixel = |x: usize| &pixels[row + x * 4..row + x * 4 + 4];
            assert_eq!(pixel(2), argb8888_bytes(colors.unfocused_titlebar));
            assert_eq!(
                pixel(width as usize / 3 + 2),
                argb8888_bytes(colors.focused_titlebar)
            );
            assert_eq!(
                pixel(width as usize * 2 / 3 + 2),
                argb8888_bytes(colors.unfocused_titlebar)
            );
            assert_eq!(
                pixel(width as usize / 3),
                argb8888_bytes(colors.focused_border)
            );
        }
    }

    #[test]
    fn configured_title_colors_reach_the_raster_and_border_buffers() {
        let colors = crate::config::Keybindings::parse(
            "[colors]\nfocused_border = '#112233'\nunfocused_border = '#445566'\nfocused_titlebar = '#102030'\nunfocused_titlebar = '#405060'\ntitle_text = '#ffffff'\n",
        )
        .unwrap()
        .decoration_colors();
        let font = TitleFont::default();
        let mut pixels = vec![0; 32 * TITLE_HEIGHT as usize * 4];
        paint_title(&mut pixels, 32, "A", true, 1, colors, &font);
        assert_eq!(&pixels[..4], &argb8888_bytes(colors.focused_titlebar));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[2] > 0x10)
        );
        paint_title(&mut pixels, 32, "", false, 1, colors, &font);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == argb8888_bytes(colors.unfocused_titlebar))
        );
        let mut decoration = Decoration::new(colors, font);
        decoration.update(true, "".into(), true, 32, 48);
        assert_eq!(
            decoration.borders[0].color(),
            colors.focused_border.solid().into()
        );
        decoration.update(true, "".into(), false, 32, 48);
        assert_eq!(
            decoration.borders[0].color(),
            colors.unfocused_border.solid().into()
        );
    }

    #[test]
    fn matching_border_and_title_colors_reach_the_display_with_the_same_rgb() {
        let mut colors = DecorationColors::default();
        colors.focused_titlebar = colors.focused_border;
        colors.unfocused_titlebar = colors.unfocused_border;
        let font = TitleFont::default();
        let mut pixels = vec![0; 64 * TITLE_HEIGHT as usize * 4];
        for (focused, color) in [
            (true, colors.focused_border),
            (false, colors.unfocused_border),
        ] {
            paint_title(&mut pixels, 64, "", focused, 1, colors, &font);
            // Argb8888's little-endian bytes are BGRA; the displayed RGB
            // must be the same RGB used by the solid-color border element.
            assert_eq!(&pixels[..4], &argb8888_bytes(color));
            assert_eq!([pixels[2], pixels[1], pixels[0]], color.0);
            let mut decoration = Decoration::new(colors, font.clone());
            decoration.update(true, "".into(), focused, 64, 80);
            assert_eq!(decoration.borders[0].color(), color.solid().into());
        }
    }

    #[test]
    fn border_wraps_title_and_client_without_a_top_gap() {
        let [left, right, top, bottom] = border_rects(100, 80);
        assert_eq!(left, (0, 0, BORDER, 80));
        assert_eq!(right, (100 - BORDER, 0, BORDER, 80));
        assert_eq!(top, (BORDER, 0, 100 - 2 * BORDER, BORDER));
        assert_eq!(bottom, (BORDER, 80 - BORDER, 100 - 2 * BORDER, BORDER));
        // Both sides span the entire titlebar and meet the top/bottom edges.
        assert!(left.3 > TITLE_HEIGHT);
        assert_eq!(top.0, left.2);
        assert_eq!(top.0 + top.2, right.0);
    }

    #[test]
    fn owner_clip_stays_on_hdmi_when_client_geometry_spills_onto_dp() {
        let owner = Rectangle::new((0, 0).into(), (3840, 2160).into());
        let local = Rectangle::new((-2, -24).into(), (4002, 2100).into());
        // Window is mapped near HDMI's right edge; render_location on HDMI
        // includes the client geometry offset, while DP subtracts 3840.
        let mapped = (3200, 40).into();
        let scale = Scale::from(1.0);
        let hdmi = owner_crop_rect(owner, mapped, local, (3202, 64).into(), scale);
        let dp = owner_crop_rect(owner, mapped, local, (-638, 64).into(), scale);
        assert_eq!(hdmi.loc, (0, 0).into());
        assert_eq!(hdmi.size, (3840, 2160).into());
        assert_eq!(dp.loc, (-3840, 0).into());
        assert_eq!(dp.size, (3840, 2160).into());
        let spill = Rectangle::new((0, 40).into(), (900, 2100).into());
        assert!(!dp.overlaps(spill));
        let local_clip = local_owner_clip(owner, mapped, local);
        let oversized_client = Rectangle::new((0, 0).into(), (1200, 2000).into());
        let visible = oversized_client.intersection(local_clip).unwrap();
        assert_eq!(visible.loc.x + visible.size.w + 3202, 3840);
    }
}

#[derive(Clone)]
pub struct DecoratedWindow {
    pub window: Window,
    decoration: Arc<Mutex<Decoration>>,
}

impl PartialEq for DecoratedWindow {
    fn eq(&self, other: &Self) -> bool {
        self.window == other.window
    }
}

impl DecoratedWindow {
    pub(crate) fn new(window: Window, colors: DecorationColors, font: TitleFont) -> Self {
        Self {
            window,
            decoration: Arc::new(Mutex::new(Decoration::new(colors, font))),
        }
    }

    pub fn update(&self, enabled: bool, title: String, focused: bool) -> bool {
        // WM layout can lead the client's committed buffer by a frame during
        // interactive resize (particularly at 60 Hz). Render borders/title
        // against the same geometry as the client surface, not the requested
        // configure size, so their edges cannot pull ahead of its pixels.
        let client = self.window.geometry();
        let size = frame_size_for_client(client.size, enabled);
        self.decoration
            .lock()
            .unwrap()
            .update(enabled, title, focused, size.w, size.h)
    }

    pub fn decorated(&self) -> bool {
        self.decoration.lock().unwrap().enabled
    }

    pub fn set_tabs(&self, tabs: Vec<(String, bool)>) -> bool {
        self.decoration.lock().unwrap().set_tabs(tabs)
    }

    /// The window belongs to one monitor even if an in-flight client buffer
    /// temporarily reaches across the boundary while resizing.
    pub fn set_owner_clip(&self, output: Rectangle<i32, Logical>, mapped_at: Point<i32, Logical>) {
        self.decoration.lock().unwrap().owner_clip = Some((output, mapped_at));
    }

    /// Floating windows live above tiled windows regardless of focus order.
    /// Return whether Space needs to reinsert the element to sort by z-index.
    pub fn set_floating(&self, floating: bool) -> bool {
        let mut decoration = self.decoration.lock().unwrap();
        let changed = decoration.floating != floating;
        decoration.floating = floating;
        changed
    }

    pub fn title_hit(&self, point: Point<f64, Logical>) -> bool {
        if !self.decorated() {
            return false;
        }
        let geometry = SpaceElement::geometry(self);
        point.x >= geometry.loc.x as f64
            && point.x < (geometry.loc.x + geometry.size.w) as f64
            && point.y >= geometry.loc.y as f64
            && point.y < (geometry.loc.y + TITLE_HEIGHT) as f64
    }
}

impl IsAlive for DecoratedWindow {
    fn alive(&self) -> bool {
        self.window.alive()
    }
}

impl SpaceElement for DecoratedWindow {
    fn geometry(&self) -> Rectangle<i32, Logical> {
        let mut geometry = self.window.geometry();
        if self.decorated() {
            geometry.loc.x -= BORDER;
            geometry.loc.y -= TITLE_HEIGHT;
            geometry.size = frame_size_for_client(geometry.size, true);
        }
        geometry
    }

    fn bbox(&self) -> Rectangle<i32, Logical> {
        let geometry = SpaceElement::geometry(self);
        let bbox = self.window.bbox_with_popups().merge(geometry);
        let Some((output, mapped_at)) = self.decoration.lock().unwrap().owner_clip else {
            return bbox;
        };
        // Space uses bbox for output-enter events and deciding which output
        // should render an element. Do not advertise an oversized resize
        // buffer on the adjacent monitor in the first place.
        let clip = local_owner_clip(output, mapped_at, geometry);
        bbox.intersection(clip)
            .unwrap_or_else(|| Rectangle::new(clip.loc, (0, 0).into()))
    }

    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        self.title_hit(*point) || SpaceElement::is_in_input_region(&self.window, point)
    }

    fn z_index(&self) -> u8 {
        if self.decoration.lock().unwrap().floating {
            1
        } else {
            0
        }
    }
    fn set_activate(&self, activated: bool) {
        SpaceElement::set_activate(&self.window, activated);
    }
    fn output_enter(&self, output: &Output, overlap: Rectangle<i32, Logical>) {
        SpaceElement::output_enter(&self.window, output, overlap);
    }
    fn output_leave(&self, output: &Output) {
        SpaceElement::output_leave(&self.window, output);
    }
    fn refresh(&self) {
        SpaceElement::refresh(&self.window);
    }
}

smithay::backend::renderer::element::render_elements! {
    pub WindowElements<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Title=MemoryRenderBufferRenderElement<R>,
    Border=SolidColorRenderElement,
}

fn local_owner_clip(
    output: Rectangle<i32, Logical>,
    mapped_at: Point<i32, Logical>,
    local_geometry: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    Rectangle::new(output.loc - mapped_at + local_geometry.loc, output.size)
}

fn owner_crop_rect(
    output: Rectangle<i32, Logical>,
    mapped_at: Point<i32, Logical>,
    local_geometry: Rectangle<i32, Logical>,
    render_location: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    // Smithay passes a per-output render_location: mapped_at minus the
    // element's local geometry origin and the output's global origin.
    let clip = local_owner_clip(output, mapped_at, local_geometry);
    Rectangle::new(
        render_location + clip.loc.to_physical_precise_round(scale),
        clip.size.to_physical_precise_round(scale),
    )
}

impl<R> AsRenderElements<R> for DecoratedWindow
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    type RenderElement = CropRenderElement<WindowElements<R>>;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        // Client popups must remain in front of the titlebar. The decoration
        // occupies the inset outside the normal toplevel surface.
        let mut elements: Vec<WindowElements<R>> = AsRenderElements::<R>::render_elements::<
            WaylandSurfaceRenderElement<R>,
        >(
            &self.window, renderer, location, scale, alpha
        )
        .into_iter()
        .map(WindowElements::Surface)
        .collect();
        let geometry = SpaceElement::geometry(self);
        let mut deco = self.decoration.lock().unwrap();
        if deco.enabled {
            let top = location + geometry.loc.to_physical_precise_round(scale);
            for ((x, y, w, h), buffer) in border_rects(geometry.size.w, geometry.size.h)
                .into_iter()
                .zip(&deco.borders)
            {
                if w > 0 && h > 0 {
                    let offset =
                        Point::<i32, Logical>::from((x, y)).to_physical_precise_round(scale);
                    elements.push(WindowElements::Border(
                        SolidColorRenderElement::from_buffer(
                            buffer,
                            top + offset,
                            scale,
                            alpha,
                            Kind::Unspecified,
                        ),
                    ));
                }
            }
            let title_scale = scale.x.round().clamp(1.0, 4.0) as i32;
            let title = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                top.to_f64(),
                deco.title_buffer(title_scale),
                Some(alpha),
                None,
                None,
                Kind::Unspecified,
            );
            match title {
                Ok(title) => elements.push(WindowElements::Title(title)),
                Err(error) => tracing::warn!(?error, "failed to upload titlebar"),
            }
        }
        let Some((output, mapped_at)) = deco.owner_clip else {
            return vec![];
        };
        let crop = owner_crop_rect(output, mapped_at, geometry, location, scale);
        elements
            .into_iter()
            .filter_map(|element| CropRenderElement::from_element(element, scale, crop))
            .map(Into::into)
            .collect()
    }
}
