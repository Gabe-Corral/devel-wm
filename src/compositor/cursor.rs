//! Cursor overlay, independent of window/layer stacking.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportAll, ImportMem, Renderer,
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
        },
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    reexports::wayland_server::Resource,
    utils::{Logical, Point, Scale, Transform},
    wayland::compositor::with_states,
};

const WIDTH: i32 = 20;
const HEIGHT: i32 = 24;

// A small, high-contrast arrow with a transparent background. The hotspot is (0, 0).
const ARROW: [&str; 24] = [
    "#                   ",
    "##                  ",
    "#.#                 ",
    "#..#                ",
    "#...#               ",
    "#....#              ",
    "#.....#             ",
    "#......#            ",
    "#.......#           ",
    "#........#          ",
    "#.........#         ",
    "#..........#        ",
    "#...........#       ",
    "#............#      ",
    "#.............#     ",
    "#..............#    ",
    "#...............#   ",
    "#.........########  ",
    "#.....##..#         ",
    "#...#  #...#        ",
    "#..#    #...#       ",
    "#.#      #...#      ",
    "##        #...#     ",
    "#          ####     ",
];

fn arrow_pixels() -> Vec<u8> {
    ARROW
        .iter()
        .flat_map(|row| row.bytes())
        .flat_map(|pixel| match pixel {
            b'#' => [0, 0, 0, 255],
            b'.' => [255, 255, 255, 255],
            _ => [0, 0, 0, 0],
        })
        .collect()
}

pub struct Cursor {
    pub image: CursorImageStatus,
    arrow: MemoryRenderBuffer,
}

smithay::backend::renderer::element::render_elements! {
    pub CursorElements<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Arrow=MemoryRenderBufferRenderElement<R>,
}

impl Cursor {
    pub fn new() -> Self {
        let mut arrow = MemoryRenderBuffer::new(
            Fourcc::Argb8888,
            (WIDTH, HEIGHT),
            1,
            Transform::Normal,
            None,
        );
        arrow
            .render()
            .draw(|pixels| {
                pixels.copy_from_slice(&arrow_pixels());
                Ok::<_, ()>(vec![])
            })
            .expect("infallible cursor draw");
        Self {
            image: CursorImageStatus::default_named(),
            arrow,
        }
    }

    pub fn elements<R>(
        &self,
        renderer: &mut R,
        location: Point<f64, Logical>,
    ) -> Vec<CursorElements<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        match &self.image {
            CursorImageStatus::Hidden => vec![],
            CursorImageStatus::Surface(surface) if surface.is_alive() => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|attributes| attributes.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let position = location - hotspot.to_f64();
                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    (position.x.round() as i32, position.y.round() as i32),
                    Scale::from(1.0),
                    1.0,
                    Kind::Cursor,
                )
            }
            // Use the built-in pointer for named cursors and destroyed surfaces.
            // Cursor themes / shape variants can be added without changing client cursors.
            _ => MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (location.x, location.y),
                &self.arrow,
                None,
                None,
                None,
                Kind::Cursor,
            )
            .map(|element| vec![CursorElements::Arrow(element)])
            .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_is_transparent_outside_the_shape_and_has_a_hotspot() {
        assert!(ARROW.iter().all(|row| row.len() == WIDTH as usize));
        let pixels = arrow_pixels();
        assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);
        assert_eq!(&pixels[..4], &[0, 0, 0, 255]);
        assert_eq!(&pixels[4..8], &[0, 0, 0, 0]);
        assert!(
            pixels
                .chunks_exact(4)
                .any(|pixel| pixel == [255, 255, 255, 255])
        );
    }
}
