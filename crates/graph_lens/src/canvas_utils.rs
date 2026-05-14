use gpui::{Bounds, Pixels, Point, Size, px};

/// Converts logical canvas coordinates to LOCAL panel-space points.
/// Use this for positioning nodes (divs) inside the panel.
pub fn to_local_screen_pt(logical: Point<f32>, zoom: f32, pan: Point<f32>) -> Point<Pixels> {
    Point {
        x: px(logical.x * zoom + pan.x),
        y: px(logical.y * zoom + pan.y),
    }
}

/// Converts logical canvas coordinates to GLOBAL window-space points.
/// Use this for painting directly to the window (e.g. paths in the canvas).
pub fn to_global_screen_pt(
    logical: Point<f32>,
    viewport_origin: Point<Pixels>,
    zoom: f32,
    pan: Point<f32>,
) -> Point<Pixels> {
    let local = to_local_screen_pt(logical, zoom, pan);
    Point {
        x: local.x + viewport_origin.x,
        y: local.y + viewport_origin.y,
    }
}

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

pub fn visible_logical_bounds(
    viewport_origin: Point<Pixels>,
    viewport_size: Size<Pixels>,
    zoom: f32,
    pan: Point<f32>,
) -> Bounds<f32> {
    // The top-left logical point is simply the window origin mapped to logical space
    let top_left = to_logical_pt(viewport_origin, viewport_origin, zoom, pan);

    let bottom_right_screen = Point {
        x: viewport_origin.x + viewport_size.width,
        y: viewport_origin.y + viewport_size.height,
    };

    let bottom_right = to_logical_pt(bottom_right_screen, viewport_origin, zoom, pan);

    Bounds {
        origin: top_left,
        size: Size {
            width: bottom_right.x - top_left.x,
            height: bottom_right.y - top_left.y,
        },
    }
}
