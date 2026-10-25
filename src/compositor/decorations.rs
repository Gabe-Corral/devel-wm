//! Per-window decoration geometry and render elements, interleaved with the
//! corresponding client surface in Space so overlapping windows stack correctly.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use ab_glyph::{Font, FontRef, ScaleFont, point};
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
    utils::{IsAlive, Logical, Physical, Point, Rectangle, Scale, Transform},
};

use smithay::backend::allocator::Fourcc;

pub const TITLE_HEIGHT: i32 = 24;
pub const BORDER: i32 = 2;

pub fn insets(decorated: bool) -> (i32, i32) {
    if decorated {
        (BORDER, TITLE_HEIGHT)
    } else {
        (0, 0)
    }
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

fn title_font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| {
        FontRef::try_from_slice(include_bytes!("../../assets/fonts/DejaVuSans.ttf"))
            .expect("bundled title font")
    })
}

fn paint_title(pixels: &mut [u8], logical_width: i32, title: &str, focused: bool, scale: i32) {
    let width = logical_width * scale;
    let height = TITLE_HEIGHT * scale;
    let background = if focused {
        [0x54, 0x40, 0x27, 0xff]
    } else {
        [0x32, 0x30, 0x2d, 0xff]
    };
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&background);
    }
    let font = title_font();
    let scaled = font.as_scaled(14.0 * scale as f32);
    let baseline = (height as f32 - (scaled.ascent() - scaled.descent())) / 2.0 + scaled.ascent();
    let padding = 8.0 * scale as f32;
    let mut x = padding;
    let mut previous = None;
    for ch in title.chars() {
        let id = font.glyph_id(ch);
        let id = if id.0 == 0 { font.glyph_id('?') } else { id };
        let kern = previous.map_or(0.0, |last| scaled.kern(last, id));
        let next_x = x + kern;
        let advance = scaled.h_advance(id);
        if next_x + advance > width as f32 - padding {
            break;
        }
        x = next_x;
        if let Some(glyph) =
            font.outline_glyph(id.with_scale_and_position(14.0 * scale as f32, point(x, baseline)))
        {
            let bounds = glyph.px_bounds();
            glyph.draw(|dx, dy, coverage| {
                let px = bounds.min.x as i32 + dx as i32;
                let py = bounds.min.y as i32 + dy as i32;
                if px >= padding as i32 && px < width - padding as i32 && py >= 0 && py < height {
                    let offset = ((py * width + px) * 4) as usize;
                    for channel in 0..3 {
                        pixels[offset + channel] = (background[channel] as f32 * (1.0 - coverage)
                            + 245.0 * coverage)
                            .round() as u8;
                    }
                }
            });
        }
        x += advance;
        previous = Some(id);
    }
}

struct Decoration {
    enabled: bool,
    floating: bool,
    title: String,
    focused: bool,
    width: i32,
    height: i32,
    owner_clip: Option<(Rectangle<i32, Logical>, Point<i32, Logical>)>,
    title_buffers: HashMap<i32, MemoryRenderBuffer>,
    borders: [SolidColorBuffer; 4],
}

impl Decoration {
    fn new() -> Self {
        Self {
            enabled: false,
            floating: false,
            title: String::new(),
            focused: false,
            width: 1,
            height: 1,
            owner_clip: None,
            title_buffers: HashMap::new(),
            borders: std::array::from_fn(|_| {
                SolidColorBuffer::new((1, 1), [0.22, 0.24, 0.28, 1.0])
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
            let color = if focused {
                [0.28, 0.48, 0.78, 1.0]
            } else {
                [0.22, 0.24, 0.28, 1.0]
            };
            for (border, (_, _, w, h)) in self.borders.iter_mut().zip(border_rects(width, height)) {
                border.update((w.max(1), h.max(1)), color);
            }
        }
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
                    paint_title(pixels, self.width, &self.title, self.focused, scale);
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
    fn title_pixels_are_clipped_and_change_with_focus() {
        let mut pixels = vec![0; 23 * TITLE_HEIGHT as usize * 4];
        paint_title(&mut pixels, 23, "overlong title", false, 1);
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [0x32, 0x30, 0x2d, 0xff])
        );
        let mut pixels = vec![0; 32 * TITLE_HEIGHT as usize * 4];
        paint_title(&mut pixels, 32, "A never fits", true, 1);
        assert_eq!(&pixels[0..4], &[0x54, 0x40, 0x27, 0xff]);
        assert!(
            pixels
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 0x54 && pixel[3] == 0xff)
        );
        assert_eq!(insets(true), (BORDER, TITLE_HEIGHT));
        assert_eq!(insets(false), (0, 0));
    }

    #[test]
    fn title_raster_is_drawn_at_physical_scale_without_resampling() {
        let mut pixels = vec![0; 64 * (TITLE_HEIGHT * 2) as usize * 4];
        paint_title(&mut pixels, 32, "A", true, 2);
        assert!(
            pixels
                .chunks_exact(4)
                .any(|pixel| { pixel[0] > 0x54 && pixel[0] < 0xf5 && pixel[3] == 0xff })
        );
        let mut decoration = Decoration::new();
        decoration.update(true, "A".into(), true, 32, 48);
        decoration.title_buffer(2);
        decoration.title_buffer(1);
        assert!(decoration.title_buffers.contains_key(&2));
        assert!(decoration.title_buffers.contains_key(&1));
        decoration.update(true, "B".into(), true, 32, 48);
        assert!(decoration.title_buffers.is_empty());
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
    pub fn new(window: Window) -> Self {
        Self {
            window,
            decoration: Arc::new(Mutex::new(Decoration::new())),
        }
    }

    pub fn update(
        &self,
        enabled: bool,
        title: String,
        focused: bool,
        width: i32,
        height: i32,
    ) -> bool {
        self.decoration
            .lock()
            .unwrap()
            .update(enabled, title, focused, width, height)
    }

    pub fn decorated(&self) -> bool {
        self.decoration.lock().unwrap().enabled
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
            geometry.size.w += BORDER * 2;
            geometry.size.h += TITLE_HEIGHT + BORDER;
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
