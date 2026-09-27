//! Per-window decoration geometry and render elements, interleaved with the
//! corresponding client surface in Space so overlapping windows stack correctly.

use std::sync::{Arc, Mutex};

use font8x8::{BASIC_FONTS, UnicodeFonts};
use smithay::{
    backend::renderer::{
        ImportAll, ImportMem, Renderer,
        element::{
            AsRenderElements, Kind,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
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

fn paint_title(pixels: &mut [u8], width: i32, title: &str, focused: bool) {
    let background = if focused {
        [0x54, 0x40, 0x27, 0xff]
    } else {
        [0x32, 0x30, 0x2d, 0xff]
    };
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&background);
    }
    for (index, ch) in title
        .chars()
        .take(((width - 16).max(0) / 8) as usize)
        .enumerate()
    {
        let glyph = BASIC_FONTS.get(ch).unwrap_or(BASIC_FONTS.get('?').unwrap());
        for (y, row) in glyph.iter().enumerate() {
            for x in 0..8 {
                if row & (1 << x) != 0 {
                    let offset = ((y + 8) * width as usize + 8 + index * 8 + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[0xf5, 0xf5, 0xf5, 0xff]);
                }
            }
        }
    }
}

struct Decoration {
    enabled: bool,
    title: String,
    focused: bool,
    width: i32,
    height: i32,
    title_buffer: MemoryRenderBuffer,
    borders: [SolidColorBuffer; 3],
}

impl Decoration {
    fn new() -> Self {
        Self {
            enabled: false,
            title: String::new(),
            focused: false,
            width: 1,
            height: 1,
            title_buffer: MemoryRenderBuffer::new(
                Fourcc::Argb8888,
                (1, TITLE_HEIGHT),
                1,
                Transform::Normal,
                None,
            ),
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
            let mut context = self.title_buffer.render();
            context.resize((width, TITLE_HEIGHT));
            context
                .draw(|pixels| {
                    paint_title(pixels, width, &self.title, focused);
                    Ok::<_, ()>(vec![Rectangle::from_size((width, TITLE_HEIGHT).into())])
                })
                .expect("infallible title draw");
            context.update_opaque_regions(Some(vec![Rectangle::from_size(
                (width, TITLE_HEIGHT).into(),
            )]));
            let color = if focused {
                [0.28, 0.48, 0.78, 1.0]
            } else {
                [0.22, 0.24, 0.28, 1.0]
            };
            for (border, size) in self.borders.iter_mut().zip([
                (BORDER, height - TITLE_HEIGHT),
                (BORDER, height - TITLE_HEIGHT),
                (width - BORDER * 2, BORDER),
            ]) {
                border.update(size, color);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_pixels_are_clipped_and_change_with_focus() {
        let mut pixels = vec![0; 23 * TITLE_HEIGHT as usize * 4];
        paint_title(&mut pixels, 23, "overlong title", false);
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [0x32, 0x30, 0x2d, 0xff])
        );
        let mut pixels = vec![0; 32 * TITLE_HEIGHT as usize * 4];
        paint_title(&mut pixels, 32, "A never fits", true);
        assert_eq!(&pixels[0..4], &[0x54, 0x40, 0x27, 0xff]);
        assert!(
            pixels
                .chunks_exact(4)
                .any(|pixel| pixel == [0xf5, 0xf5, 0xf5, 0xff])
        );
        assert_eq!(insets(true), (BORDER, TITLE_HEIGHT));
        assert_eq!(insets(false), (0, 0));
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
        self.window
            .bbox_with_popups()
            .merge(SpaceElement::geometry(self))
    }

    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        self.title_hit(*point) || SpaceElement::is_in_input_region(&self.window, point)
    }

    fn z_index(&self) -> u8 {
        SpaceElement::z_index(&self.window)
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

impl<R> AsRenderElements<R> for DecoratedWindow
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    type RenderElement = WindowElements<R>;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        // Client popups must remain in front of the titlebar. The decoration
        // occupies the inset outside the normal toplevel surface.
        let mut elements: Vec<C> = AsRenderElements::<R>::render_elements::<
            WaylandSurfaceRenderElement<R>,
        >(&self.window, renderer, location, scale, alpha)
        .into_iter()
        .map(|element| WindowElements::Surface(element).into())
        .collect();
        let geometry = SpaceElement::geometry(self);
        let deco = self.decoration.lock().unwrap();
        if deco.enabled {
            let top = location + geometry.loc.to_physical_precise_round(scale);
            let title = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                top.to_f64(),
                &deco.title_buffer,
                Some(alpha),
                None,
                None,
                Kind::Unspecified,
            );
            match title {
                Ok(title) => elements.push(WindowElements::Title(title).into()),
                Err(error) => tracing::warn!(?error, "failed to upload titlebar"),
            }
            for ((x, y, w, h), buffer) in [
                (0, TITLE_HEIGHT, BORDER, geometry.size.h - TITLE_HEIGHT),
                (
                    geometry.size.w - BORDER,
                    TITLE_HEIGHT,
                    BORDER,
                    geometry.size.h - TITLE_HEIGHT,
                ),
                (
                    BORDER,
                    geometry.size.h - BORDER,
                    geometry.size.w - BORDER * 2,
                    BORDER,
                ),
            ]
            .into_iter()
            .zip(&deco.borders)
            {
                if w > 0 && h > 0 {
                    let offset =
                        Point::<i32, Logical>::from((x, y)).to_physical_precise_round(scale);
                    elements.push(
                        WindowElements::Border(SolidColorRenderElement::from_buffer(
                            buffer,
                            top + offset,
                            scale,
                            alpha,
                            Kind::Unspecified,
                        ))
                        .into(),
                    );
                }
            }
        }
        elements
    }
}
