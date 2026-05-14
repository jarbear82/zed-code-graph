use gpui::{Pixels, Point, px};

/// Converts a logical canvas coordinate (from GraphEngine) to a screen-space point.
///
/// This is used to accurately place GPUI elements (`div().absolute()`) over the
/// infinite canvas based on the current camera state.
///
/// Formula: screen = (logical * zoom) + pan + viewport_origin
pub fn to_screen_pt(
    logical: Point<f32>,
    viewport_origin: Point<Pixels>,
    zoom: f32,
    pan: Point<f32>,
) -> Point<Pixels> {
    // In GPUI, Pixels can typically be converted to f32 via `.into()`
    let origin_x: f32 = viewport_origin.x.into();
    let origin_y: f32 = viewport_origin.y.into();

    Point {
        x: px(logical.x * zoom + pan.x + origin_x),
        y: px(logical.y * zoom + pan.y + origin_y),
    }
}

/// Converts a screen-space point (from a mouse event) to a logical canvas coordinate.
///
/// This is used to map where the user clicked on the monitor to the corresponding
/// coordinate in the physics engine.
///
/// Formula: logical = (screen - viewport_origin - pan) / zoom
pub fn to_logical_pt(
    screen: Point<Pixels>,
    viewport_origin: Point<Pixels>,
    zoom: f32,
    pan: Point<f32>,
) -> Point<f32> {
    let screen_x: f32 = screen.x.into();
    let screen_y: f32 = screen.y.into();
    let origin_x: f32 = viewport_origin.x.into();
    let origin_y: f32 = viewport_origin.y.into();

    Point {
        x: (screen_x - origin_x - pan.x) / zoom,
        y: (screen_y - origin_y - pan.y) / zoom,
    }
}

/// Calculates the visible bounds of the canvas in logical coordinates.
///
/// Useful for viewport culling: avoiding rendering nodes/edges that are
/// currently panned or zoomed out of view.
pub fn visible_logical_bounds(
    viewport_origin: Point<Pixels>,
    viewport_size: gpui::Size<Pixels>,
    zoom: f32,
    pan: Point<f32>,
) -> gpui::Bounds<f32> {
    let top_left = to_logical_pt(viewport_origin, viewport_origin, zoom, pan);

    let bottom_right_screen = Point {
        x: viewport_origin.x + viewport_size.width,
        y: viewport_origin.y + viewport_size.height,
    };

    let bottom_right = to_logical_pt(bottom_right_screen, viewport_origin, zoom, pan);

    gpui::Bounds {
        origin: top_left,
        size: gpui::Size {
            width: bottom_right.x - top_left.x,
            height: bottom_right.y - top_left.y,
        },
    }
}
