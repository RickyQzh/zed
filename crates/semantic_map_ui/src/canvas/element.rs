//! Canvas paint helpers.
//!
//! Phase A cards are positioned `div` children (hit-testing via GPUI layout).
//! [`SemanticMapCanvasElement`] paints DependsOn (and other) edge polylines
//! with a thin `canvas` overlay.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{App, Bounds, IntoElement, PathBuilder, Pixels, Point, Window, canvas, point, px};
use ui::prelude::*;

use super::skins::vibe::VibeSkin;
use crate::SceneEdge;

/// Edge overlay for the div-based semantic map canvas.
#[derive(IntoElement)]
pub struct SemanticMapCanvasElement {
    edges: Vec<SceneEdge>,
    pan: Point<f32>,
    zoom: f32,
    viewport_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl SemanticMapCanvasElement {
    pub fn new(
        edges: Vec<SceneEdge>,
        pan: Point<f32>,
        zoom: f32,
        viewport_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    ) -> Self {
        Self {
            edges,
            pan,
            zoom,
            viewport_bounds,
        }
    }
}

impl RenderOnce for SemanticMapCanvasElement {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let edges = self.edges;
        let pan = self.pan;
        let zoom = self.zoom;
        let viewport_bounds = self.viewport_bounds;
        let color = VibeSkin::edge_color(cx);

        canvas(
            move |bounds, _, _| {
                viewport_bounds.set(Some(bounds));
            },
            move |bounds, _, window, _cx| {
                for edge in &edges {
                    if edge.routed_path.len() < 2 {
                        continue;
                    }
                    let mut builder = PathBuilder::stroke(px(1.5));
                    for (index, logical) in edge.routed_path.iter().enumerate() {
                        let screen = map_point(*logical, pan, zoom, bounds);
                        if index == 0 {
                            builder.move_to(screen);
                        } else {
                            builder.line_to(screen);
                        }
                    }
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, color);
                    }
                }
            },
        )
        .size_full()
    }
}

fn map_point(
    logical: Point<f32>,
    pan: Point<f32>,
    zoom: f32,
    bounds: Bounds<Pixels>,
) -> Point<Pixels> {
    point(
        bounds.origin.x + px(logical.x * zoom + pan.x),
        bounds.origin.y + px(logical.y * zoom + pan.y),
    )
}
