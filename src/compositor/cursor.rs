//! Xcursor-themed pointer overlay, independent of window/layer stacking.

use std::{collections::HashMap, fs, sync::Mutex};

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
    input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    reexports::wayland_server::Resource,
    utils::{Physical, Point, Scale, Transform},
    wayland::compositor::with_states,
};

const WIDTH: i32 = 20;
const HEIGHT: i32 = 24;
const DEFAULT_SIZE: u32 = 24;

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
    themes: Vec<xcursor::CursorTheme>,
    size: u32,
    themed: Mutex<HashMap<(CursorIcon, i32), Option<ThemedCursor>>>,
}

struct ThemedCursor {
    buffer: MemoryRenderBuffer,
    hotspot: (f64, f64),
}

fn select_image(images: &[xcursor::parser::Image], size: u32) -> Option<&xcursor::parser::Image> {
    images.iter().min_by_key(|image| image.size.abs_diff(size))
}

fn theme_names() -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(theme) = std::env::var("XCURSOR_THEME")
        && !theme.is_empty()
    {
        names.push(theme);
    }
    // Respect the desktop's default first. Some Manjaro installations still
    // point "default" at the old xcursor-breeze alias, while the actual
    // cursor files are installed as breeze-cursors.
    for name in ["default", "breeze-cursors"] {
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    }
    names
}

fn cursor_size() -> u32 {
    std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|size| (1..=128).contains(size))
        .unwrap_or(DEFAULT_SIZE)
}

fn load_icon(
    themes: &[xcursor::CursorTheme],
    icon: CursorIcon,
    size: u32,
    scale: i32,
) -> Option<ThemedCursor> {
    let names = if icon == CursorIcon::Default {
        vec!["default", "left_ptr"]
    } else {
        vec![icon.name()]
    };
    for theme in themes {
        for name in &names {
            let Some(path) = theme.load_icon(name) else {
                continue;
            };
            let Ok(contents) = fs::read(&path) else {
                continue;
            };
            let Some(images) = xcursor::parser::parse_xcursor(&contents) else {
                continue;
            };
            let Some(image) = select_image(&images, size.saturating_mul(scale as u32)) else {
                continue;
            };
            // Theme files come from the environment. Keep pointer buffers
            // bounded even when a malformed theme advertises huge images.
            if image.width > 256 || image.height > 256 {
                continue;
            }
            let (width, height) = (image.width as i32, image.height as i32);
            // Xcursor stores premultiplied ARGB32 as native little-endian
            // bytes (BGRA), matching the renderer's Argb8888 input.
            let mut buffer = MemoryRenderBuffer::new(
                Fourcc::Argb8888,
                (width, height),
                scale,
                Transform::Normal,
                None,
            );
            buffer
                .render()
                .draw(|pixels| {
                    pixels.copy_from_slice(&image.pixels_rgba);
                    Ok::<_, ()>(vec![])
                })
                .expect("infallible cursor draw");
            return Some(ThemedCursor {
                buffer,
                hotspot: (
                    f64::from(image.xhot) / f64::from(scale),
                    f64::from(image.yhot) / f64::from(scale),
                ),
            });
        }
    }
    if icon != CursorIcon::Default {
        return load_icon(themes, CursorIcon::Default, size, scale);
    }
    None
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
            themes: theme_names()
                .iter()
                .map(|name| xcursor::CursorTheme::load(name))
                .collect(),
            size: cursor_size(),
            themed: Mutex::new(HashMap::new()),
        }
    }

    /// Custom render elements use output-local physical positions, unlike
    /// the logical cursor position stored for Wayland hit testing.
    pub fn elements<R>(
        &self,
        renderer: &mut R,
        location: Point<f64, Physical>,
        output_scale: f64,
        override_icon: Option<CursorIcon>,
    ) -> Vec<CursorElements<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        let override_image = override_icon.map(CursorImageStatus::Named);
        match override_image.as_ref().unwrap_or(&self.image) {
            CursorImageStatus::Hidden => vec![],
            CursorImageStatus::Surface(surface) if surface.is_alive() => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|attributes| attributes.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let position = location - hotspot.to_f64().to_physical(output_scale);
                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    (position.x.round() as i32, position.y.round() as i32),
                    Scale::from(output_scale),
                    1.0,
                    Kind::Cursor,
                )
            }
            status => {
                let icon = match status {
                    CursorImageStatus::Named(icon) => *icon,
                    _ => CursorIcon::Default,
                };
                let scale = output_scale.round().clamp(1.0, 4.0) as i32;
                let mut themed = self.themed.lock().unwrap();
                let cursor = themed
                    .entry((icon, scale))
                    .or_insert_with(|| load_icon(&self.themes, icon, self.size, scale));
                let buffer = cursor
                    .as_ref()
                    .map(|cursor| &cursor.buffer)
                    .unwrap_or(&self.arrow);
                let hotspot = cursor
                    .as_ref()
                    .map(|cursor| cursor.hotspot)
                    .unwrap_or((0.0, 0.0));
                let position = (
                    location.x - hotspot.0 * output_scale,
                    location.y - hotspot.1 * output_scale,
                );
                MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    position,
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .map(|element| vec![CursorElements::Arrow(element)])
                .unwrap_or_default()
            }
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

    #[test]
    fn selects_the_nearest_native_resolution_for_each_output_scale() {
        let images: Vec<_> = [24, 36, 48]
            .into_iter()
            .map(|size| xcursor::parser::Image {
                size,
                width: size,
                height: size,
                xhot: 4,
                yhot: 4,
                delay: 0,
                pixels_rgba: Vec::new(),
                pixels_argb: Vec::new(),
            })
            .collect();
        assert_eq!(select_image(&images, 24).unwrap().size, 24);
        assert_eq!(select_image(&images, 48).unwrap().size, 48);
        assert_eq!(select_image(&images, 72).unwrap().size, 48);
    }

    #[test]
    fn installed_breeze_cursor_retains_its_hotspot_and_transparency() {
        let theme = xcursor::CursorTheme::load("breeze-cursors");
        let Some(path) = theme.load_icon("default") else {
            return; // The compositor also supports systems without Breeze.
        };
        let images = xcursor::parser::parse_xcursor(&fs::read(path).unwrap()).unwrap();
        let image = select_image(&images, DEFAULT_SIZE).unwrap();
        let cursor = load_icon(&[theme], CursorIcon::Default, DEFAULT_SIZE, 1).unwrap();
        assert_eq!(
            cursor.hotspot,
            (f64::from(image.xhot), f64::from(image.yhot))
        );
        let scaled = load_icon(
            &[xcursor::CursorTheme::load("breeze-cursors")],
            CursorIcon::Default,
            DEFAULT_SIZE,
            2,
        )
        .unwrap();
        let physical = select_image(&images, DEFAULT_SIZE * 2).unwrap();
        assert_eq!(
            scaled.hotspot,
            (
                f64::from(physical.xhot) / 2.0,
                f64::from(physical.yhot) / 2.0
            )
        );
        assert!(image.pixels_rgba.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(
            image
                .pixels_rgba
                .chunks_exact(4)
                .any(|pixel| pixel[3] == 255)
        );
    }

    #[test]
    fn installed_breeze_has_a_horizontal_resize_cursor() {
        let theme = xcursor::CursorTheme::load("breeze-cursors");
        let Some(default) = theme.load_icon("default") else {
            return;
        };
        let horizontal = theme
            .load_icon("ew-resize")
            .expect("Breeze horizontal resize icon");
        assert_ne!(fs::read(default).unwrap(), fs::read(horizontal).unwrap());
        assert!(load_icon(&[theme], CursorIcon::EwResize, DEFAULT_SIZE, 1).is_some());
    }
}
