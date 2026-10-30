//! Photoshop's keyboard scrolling ("Keys for viewing images"): Page Up / Page Down scroll the view
//! up or down one screen, ⌘/Ctrl + Page Up / Page Down one screen left or right, Home shows the
//! image's upper-left corner and End its lower-right corner. Pure view state, so these are shell
//! commands (`menus::UI_COMMANDS`), rebindable like any other. The view stops at the image's
//! edges, as with Overscroll off.

use serde_json::{Value, json};

use crate::PhotocraftApp;

/// The scroll commands (their default keys are in `menus::UI_COMMANDS`).
pub const COMMANDS: [&str; 6] =
    ["view.scroll.pageUp", "view.scroll.pageDown", "view.scroll.pageLeft", "view.scroll.pageRight", "view.scroll.home", "view.scroll.end"];

pub fn handles(id: &str) -> bool {
    COMMANDS.contains(&id)
}

/// Scrolls the active document's view.
pub fn run(app: &mut PhotocraftApp, id: &str) -> Result<Value, String> {
    let i = app.session.active_index().ok_or("no document open")?;
    let st = app.session.active().ok_or("no document open")?;
    let (w, h) = (st.doc.size.width as f32, st.doc.size.height as f32);
    let size = app.last_canvas_rect.size();
    let zoom = app.point_zoom();
    if !(zoom.is_finite() && zoom > 0.0 && size.x > 0.0 && size.y > 0.0) {
        return Err("the canvas is not shown".into());
    }
    // A turned or mirrored view: the corner keys go to the canvas corner the document runs on
    // from, and the screens follow the view's axes (`page_keys`).
    let turned = app.ui.views.get(i).is_some_and(|v| v.rotation.abs() > 0.05) || app.ui.view.flip_horizontal;
    if turned {
        let screen = size;
        let key = match id {
            "view.scroll.pageUp" => crate::page_keys::PageKey::Scroll(egui::vec2(0.0, -screen.y)),
            "view.scroll.pageDown" => crate::page_keys::PageKey::Scroll(egui::vec2(0.0, screen.y)),
            "view.scroll.pageLeft" => crate::page_keys::PageKey::Scroll(egui::vec2(-screen.x, 0.0)),
            "view.scroll.pageRight" => crate::page_keys::PageKey::Scroll(egui::vec2(screen.x, 0.0)),
            "view.scroll.home" => crate::page_keys::PageKey::Corner { end: false },
            "view.scroll.end" => crate::page_keys::PageKey::Corner { end: true },
            _ => return Err(format!("{id} is not a scroll command")),
        };
        let rect = crate::rulers::content_rect(app, app.last_canvas_rect);
        crate::page_keys::apply(app, &[key], rect);
        return Ok(json!({ "center": app.ui.views.get(i).map(|v| v.center) }));
    }
    // One screen, in document pixels.
    let (sw, sh) = (size.x / zoom, size.y / zoom);
    let view = app.ui.views.get_mut(i).ok_or("no view for the document")?;
    let mut c = view.center;
    match id {
        "view.scroll.pageUp" => c[1] -= sh,
        "view.scroll.pageDown" => c[1] += sh,
        "view.scroll.pageLeft" => c[0] -= sw,
        "view.scroll.pageRight" => c[0] += sw,
        "view.scroll.home" => c = [sw / 2.0, sh / 2.0],
        "view.scroll.end" => c = [w - sw / 2.0, h - sh / 2.0],
        _ => return Err(format!("{id} is not a scroll command")),
    }
    view.center = [crate::scrollbars::clamp_axis(c[0], w, size.x, zoom), crate::scrollbars::clamp_axis(c[1], h, size.y, zoom)];
    view.fit_pending = false;
    view.fill_pending = false;
    Ok(json!({ "center": view.center }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1000 × 800 document at 100 % in a 200 × 100 point canvas.
    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 1000, "height": 800})).unwrap();
        app.last_canvas_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        let v = &mut app.ui.views[0];
        v.zoom = 1.0;
        v.center = [500.0, 400.0];
        v.fit_pending = false;
        app
    }

    #[test]
    fn page_keys_scroll_one_screen_and_stop_at_the_edges() {
        let mut app = app();
        let ctx = egui::Context::default();
        let go = |app: &mut PhotocraftApp, id: &str| {
            crate::menus::invoke(app, &ctx, id, json!({})).unwrap();
            app.ui.views[0].center
        };
        assert_eq!(go(&mut app, "view.scroll.pageDown"), [500.0, 500.0]);
        assert_eq!(go(&mut app, "view.scroll.pageUp"), [500.0, 400.0]);
        assert_eq!(go(&mut app, "view.scroll.pageRight"), [700.0, 400.0]);
        assert_eq!(go(&mut app, "view.scroll.pageLeft"), [500.0, 400.0]);
        assert_eq!(go(&mut app, "view.scroll.home"), [100.0, 50.0]);
        assert_eq!(go(&mut app, "view.scroll.pageUp"), [100.0, 50.0], "already at the top");
        assert_eq!(go(&mut app, "view.scroll.end"), [900.0, 750.0]);
        assert_eq!(go(&mut app, "view.scroll.pageDown"), [900.0, 750.0], "already at the bottom");
    }

    #[test]
    fn scroll_needs_a_document_and_a_canvas() {
        let mut empty = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        assert!(run(&mut empty, "view.scroll.home").is_err());
        let mut app = app();
        assert!(run(&mut app, "view.scroll.sideways").is_err());
        app.last_canvas_rect = egui::Rect::NOTHING;
        assert!(run(&mut app, "view.scroll.home").is_err());
    }
}
