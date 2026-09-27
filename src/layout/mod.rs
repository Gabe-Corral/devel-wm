//! Pure output-local logical geometry. No backend, renderer, or script callbacks.

use crate::wm::WindowId;

/// A half-open logical rectangle: `[x, x + width) × [y, y + height)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizeSide {
    Start,
    End,
}

/// Resize from the chosen edges, holding the opposite corner fixed. Clamp
/// dimensions and coordinates before converting back to i32.
pub fn resize_floating(
    origin: Rect,
    dx: i64,
    dy: i64,
    horizontal: ResizeSide,
    vertical: ResizeSide,
) -> Rect {
    fn axis(start: i32, size: i32, delta: i64, side: ResizeSide, minimum: i32) -> (i32, i32) {
        if delta == 0 {
            return (start, size);
        }
        let start = i64::from(start);
        let end = start + i64::from(size);
        let (new_start, new_end) = match side {
            ResizeSide::Start => {
                let min = i64::from(minimum).min(end - i64::from(i32::MIN));
                (
                    start.saturating_add(delta).clamp(
                        (end - i64::from(i32::MAX)).max(i64::from(i32::MIN)),
                        end - min,
                    ),
                    end,
                )
            }
            ResizeSide::End => {
                let min = i64::from(minimum).min(i64::from(i32::MAX) - start);
                (
                    start,
                    end.saturating_add(delta).clamp(
                        start + min,
                        (start + i64::from(i32::MAX)).min(i64::from(i32::MAX)),
                    ),
                )
            }
        };
        ((new_start as i32), (new_end - new_start) as i32)
    }

    let (x, width) = axis(origin.x, origin.width, dx, horizontal, 120);
    let (y, height) = axis(origin.y, origin.height, dy, vertical, 80);
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Fraction of the space between columns given to the master column, in /1000.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MasterRatio(u16);

impl MasterRatio {
    /// Both columns must have a positive share when a stack exists.
    pub const fn new(thousandths: u16) -> Option<Self> {
        if thousandths > 0 && thousandths < 1000 {
            Some(Self(thousandths))
        } else {
            None
        }
    }

    pub const fn thousandths(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutSettings {
    pub master_ratio: MasterRatio,
    pub master_count: usize,
    /// Logical pixels around tiles and between columns/rows.
    pub gap: i32,
    /// Logical pixels inset from each tile frame to its client content.
    pub border: i32,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            master_ratio: MasterRatio(600),
            master_count: 1,
            gap: 8,
            border: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutError {
    InvalidArea,
    InvalidSettings,
    TooManyWindows,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Placement {
    pub window: WindowId,
    /// Tile including its future decoration/border region.
    pub frame: Rect,
    /// Inset rectangle for the client, not including the border.
    pub content: Rect,
}

/// Arrange distinct, ordered tiled IDs; floating and fullscreen windows are
/// excluded by the caller. Validation is independent of the number of windows.
/// Degenerate areas yield zero-sized rectangles rather than negative geometry.
pub fn master_stack(
    usable: Rect,
    tiled: &[WindowId],
    settings: LayoutSettings,
) -> Result<Vec<Placement>, LayoutError> {
    if usable.width < 0
        || usable.height < 0
        || usable.x.checked_add(usable.width).is_none()
        || usable.y.checked_add(usable.height).is_none()
    {
        return Err(LayoutError::InvalidArea);
    }
    if settings.master_count == 0 || settings.gap < 0 || settings.border < 0 {
        return Err(LayoutError::InvalidSettings);
    }
    let count = i64::try_from(tiled.len()).map_err(|_| LayoutError::TooManyWindows)?;
    let master_count = tiled.len().min(settings.master_count);
    let mut result = Vec::with_capacity(tiled.len());
    if count == 0 {
        return Ok(result);
    }

    let width = i64::from(usable.width);
    let height = i64::from(usable.height);
    let gap = i64::from(settings.gap);
    let outer_x = gap.min(width / 2);
    let outer_y = gap.min(height / 2);
    let inner_x = i64::from(usable.x) + outer_x;
    let inner_y = i64::from(usable.y) + outer_y;
    let inner_width = width - 2 * outer_x;
    let inner_height = height - 2 * outer_y;

    if master_count == tiled.len() {
        place_column(
            &mut result,
            tiled,
            inner_x,
            inner_y,
            inner_width,
            inner_height,
            settings,
        )?;
    } else {
        let column_gap = gap.min(inner_width);
        let shared_width = inner_width - column_gap;
        let master_width = shared_width * i64::from(settings.master_ratio.thousandths()) / 1000;
        place_column(
            &mut result,
            &tiled[..master_count],
            inner_x,
            inner_y,
            master_width,
            inner_height,
            settings,
        )?;
        place_column(
            &mut result,
            &tiled[master_count..],
            inner_x + master_width + column_gap,
            inner_y,
            shared_width - master_width,
            inner_height,
            settings,
        )?;
    }
    Ok(result)
}

/// One pass per column, with fixed-point division and a single result allocation.
fn place_column(
    out: &mut Vec<Placement>,
    windows: &[WindowId],
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    settings: LayoutSettings,
) -> Result<(), LayoutError> {
    let count = i64::try_from(windows.len()).map_err(|_| LayoutError::TooManyWindows)?;
    let row_gap = if count > 1 {
        i64::from(settings.gap).min(height / (count - 1))
    } else {
        0
    };
    let pixels = height - row_gap * (count - 1);
    let base = pixels / count;
    let extra = pixels % count;
    let mut top = y;
    for (index, &window) in windows.iter().enumerate() {
        let row_height = base + i64::from((index as i64) < extra);
        let frame = Rect {
            x: i32::try_from(x).map_err(|_| LayoutError::InvalidArea)?,
            y: i32::try_from(top).map_err(|_| LayoutError::InvalidArea)?,
            width: i32::try_from(width).map_err(|_| LayoutError::InvalidArea)?,
            height: i32::try_from(row_height).map_err(|_| LayoutError::InvalidArea)?,
        };
        let inset_x = i64::from(settings.border).min(width / 2);
        let inset_y = i64::from(settings.border).min(row_height / 2);
        let content = Rect {
            x: i32::try_from(x + inset_x).map_err(|_| LayoutError::InvalidArea)?,
            y: i32::try_from(top + inset_y).map_err(|_| LayoutError::InvalidArea)?,
            width: i32::try_from(width - 2 * inset_x).map_err(|_| LayoutError::InvalidArea)?,
            height: i32::try_from(row_height - 2 * inset_y)
                .map_err(|_| LayoutError::InvalidArea)?,
        };
        out.push(Placement {
            window,
            frame,
            content,
        });
        top += row_height + row_gap;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wm::{WindowOwner, WmState, WorkspaceId};

    fn ids(count: usize) -> Vec<WindowId> {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        (0..count)
            .map(|_| {
                wm.add_window(WindowOwner {
                    monitor,
                    workspace: WorkspaceId::FIRST,
                })
                .unwrap()
            })
            .collect()
    }

    fn area() -> Rect {
        Rect {
            x: 10,
            y: 20,
            width: 100,
            height: 80,
        }
    }

    #[test]
    fn floating_resize_keeps_opposite_edges_fixed_and_clamps_minimums() {
        let origin = Rect {
            x: 100,
            y: 200,
            width: 300,
            height: 220,
        };
        assert_eq!(
            resize_floating(origin, 40, 30, ResizeSide::Start, ResizeSide::Start),
            Rect {
                x: 140,
                y: 230,
                width: 260,
                height: 190
            }
        );
        assert_eq!(
            resize_floating(origin, 40, 30, ResizeSide::End, ResizeSide::End),
            Rect {
                x: 100,
                y: 200,
                width: 340,
                height: 250
            }
        );
        assert_eq!(
            resize_floating(origin, 10_000, 10_000, ResizeSide::Start, ResizeSide::Start),
            Rect {
                x: 280,
                y: 340,
                width: 120,
                height: 80
            }
        );
        assert_eq!(
            resize_floating(origin, -10_000, -10_000, ResizeSide::End, ResizeSide::End),
            Rect {
                x: 100,
                y: 200,
                width: 120,
                height: 80
            }
        );
        assert_eq!(
            resize_floating(origin, 0, 0, ResizeSide::End, ResizeSide::Start),
            origin
        );
        let near_limit = Rect {
            x: i32::MAX - 5,
            y: i32::MIN,
            width: 5,
            height: 100,
        };
        let resized = resize_floating(
            near_limit,
            i64::MAX,
            i64::MIN,
            ResizeSide::End,
            ResizeSide::Start,
        );
        assert!(resized.x.checked_add(resized.width).is_some());
        assert!(resized.y.checked_add(resized.height).is_some());
        assert!(resized.width > 0 && resized.height > 0);
    }

    #[test]
    fn zero_and_one_window() {
        let settings = LayoutSettings::default();
        assert!(master_stack(area(), &[], settings).unwrap().is_empty());
        let windows = ids(1);
        let result = master_stack(area(), &windows, settings).unwrap();
        assert_eq!(result[0].window, windows[0]);
        assert_eq!(
            result[0].frame,
            Rect {
                x: 18,
                y: 28,
                width: 84,
                height: 64
            }
        );
        assert_eq!(
            result[0].content,
            Rect {
                x: 20,
                y: 30,
                width: 80,
                height: 60
            }
        );
    }

    #[test]
    fn master_and_stack_use_ratio_and_stable_order() {
        let windows = ids(3);
        let settings = LayoutSettings {
            gap: 4,
            border: 1,
            ..Default::default()
        };
        let result = master_stack(area(), &windows, settings).unwrap();
        assert_eq!(result.iter().map(|p| p.window).collect::<Vec<_>>(), windows);
        assert_eq!(
            result[0].frame,
            Rect {
                x: 14,
                y: 24,
                width: 52,
                height: 72
            }
        );
        assert_eq!(
            result[1].frame,
            Rect {
                x: 70,
                y: 24,
                width: 36,
                height: 34
            }
        );
        assert_eq!(
            result[2].frame,
            Rect {
                x: 70,
                y: 62,
                width: 36,
                height: 34
            }
        );
    }

    #[test]
    fn multiple_masters_and_uneven_pixels() {
        let windows = ids(5);
        let settings = LayoutSettings {
            master_count: 2,
            master_ratio: MasterRatio::new(500).unwrap(),
            gap: 1,
            border: 0,
        };
        let result = master_stack(
            Rect {
                x: 0,
                y: 0,
                width: 21,
                height: 14,
            },
            &windows,
            settings,
        )
        .unwrap();
        assert_eq!(
            result.iter().map(|p| p.frame.height).collect::<Vec<_>>(),
            [6, 5, 4, 3, 3]
        );
        assert_eq!(result[0].frame.width, 9);
        assert_eq!(result[2].frame.width, 9);
        assert_eq!(result[2].frame.y, 1);
        assert_eq!(result[4].frame.y, 10);
    }

    #[test]
    fn tiny_areas_clamp_gaps_and_borders_without_negative_geometry() {
        let windows = ids(6);
        let settings = LayoutSettings {
            gap: 100,
            border: 100,
            ..Default::default()
        };
        for width in 0..=4 {
            for height in 0..=4 {
                let area = Rect {
                    x: -10,
                    y: 15,
                    width,
                    height,
                };
                let result = master_stack(area, &windows, settings).unwrap();
                assert_eq!(result.len(), windows.len());
                for item in &result {
                    assert!(item.frame.width >= 0 && item.frame.height >= 0);
                    assert!(item.content.width >= 0 && item.content.height >= 0);
                    assert!(item.frame.x >= area.x && item.frame.y >= area.y);
                    assert!(item.frame.x + item.frame.width <= area.x + area.width);
                    assert!(item.frame.y + item.frame.height <= area.y + area.height);
                    assert!(item.content.x >= item.frame.x && item.content.y >= item.frame.y);
                    assert!(item.content.x + item.content.width <= item.frame.x + item.frame.width);
                    assert!(
                        item.content.y + item.content.height <= item.frame.y + item.frame.height
                    );
                }
                for (i, a) in result.iter().enumerate() {
                    for b in result.iter().skip(i + 1) {
                        assert!(
                            a.frame.width == 0
                                || a.frame.height == 0
                                || b.frame.width == 0
                                || b.frame.height == 0
                                || a.frame.x + a.frame.width <= b.frame.x
                                || b.frame.x + b.frame.width <= a.frame.x
                                || a.frame.y + a.frame.height <= b.frame.y
                                || b.frame.y + b.frame.height <= a.frame.y
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn varied_counts_and_ratios_remain_deterministic_and_non_overlapping() {
        let windows = ids(12);
        for count in 0..=windows.len() {
            for ratio in [1, 333, 600, 999] {
                for (width, height) in [(1, 1), (27, 19), (1366, 768)] {
                    let usable = Rect {
                        x: -30,
                        y: 16,
                        width,
                        height,
                    };
                    let settings = LayoutSettings {
                        master_ratio: MasterRatio::new(ratio).unwrap(),
                        master_count: 2,
                        gap: 3,
                        border: 1,
                    };
                    let first = master_stack(usable, &windows[..count], settings).unwrap();
                    assert_eq!(
                        first,
                        master_stack(usable, &windows[..count], settings).unwrap()
                    );
                    assert_eq!(first.len(), count);
                    for (index, a) in first.iter().enumerate() {
                        assert_eq!(a.window, windows[index]);
                        assert!(a.frame.x >= usable.x && a.frame.y >= usable.y);
                        assert!(a.frame.x + a.frame.width <= usable.x + usable.width);
                        assert!(a.frame.y + a.frame.height <= usable.y + usable.height);
                        for b in first.iter().skip(index + 1) {
                            assert!(
                                a.frame.width == 0
                                    || a.frame.height == 0
                                    || b.frame.width == 0
                                    || b.frame.height == 0
                                    || a.frame.x + a.frame.width <= b.frame.x
                                    || b.frame.x + b.frame.width <= a.frame.x
                                    || a.frame.y + a.frame.height <= b.frame.y
                                    || b.frame.y + b.frame.height <= a.frame.y
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rejects_invalid_inputs() {
        let windows = ids(2);
        assert_eq!(MasterRatio::new(0), None);
        assert_eq!(MasterRatio::new(1000), None);
        let defaults = LayoutSettings::default();
        assert_eq!(
            master_stack(
                Rect {
                    width: -1,
                    ..area()
                },
                &windows,
                defaults
            ),
            Err(LayoutError::InvalidArea)
        );
        assert_eq!(
            master_stack(
                Rect {
                    x: i32::MAX,
                    width: 1,
                    ..area()
                },
                &windows,
                defaults
            ),
            Err(LayoutError::InvalidArea)
        );
        assert_eq!(
            master_stack(
                area(),
                &windows,
                LayoutSettings {
                    master_count: 0,
                    ..defaults
                }
            ),
            Err(LayoutError::InvalidSettings)
        );
        assert_eq!(
            master_stack(
                area(),
                &windows,
                LayoutSettings {
                    gap: -1,
                    ..defaults
                }
            ),
            Err(LayoutError::InvalidSettings)
        );
        assert_eq!(
            master_stack(
                area(),
                &windows,
                LayoutSettings {
                    border: -1,
                    ..defaults
                }
            ),
            Err(LayoutError::InvalidSettings)
        );
    }

    #[test]
    fn many_windows_return_one_placement_each() {
        let windows = ids(500);
        let result = master_stack(
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            &windows,
            LayoutSettings::default(),
        )
        .unwrap();
        assert_eq!(result.len(), windows.len());
        assert_eq!(result.last().unwrap().window, *windows.last().unwrap());
    }
}
