//! fCoSE-rs — three-phase compound graph layout engine.
//!
//! Phase I  → spectral::SpectralLayout::apply     (SSDE draft layout)
//! Phase II → constraints::ConstraintPhase::run   (transformation + enforcement)
//! Phase III→ physics::PhysicsEngine::tick         (force-directed polishing)

mod constraints;
mod graph;
mod math;
mod physics;
mod spectral;

use constraints::{AlignmentConstraint, ConstraintPhase, FixedConstraint, RelativeConstraint};
use graph::CompoundGraph;
use physics::{LayoutState, PhysicsEngine};
use spectral::SpectralLayout;

/// Run all three phases to completion and return.
pub fn run_layout(
    graph: &mut CompoundGraph,
    fixed: &[FixedConstraint],
    alignment: &[AlignmentConstraint],
    relative: &[RelativeConstraint],
    max_iter: usize,
) {
    // Phase I — spectral draft layout.
    SpectralLayout::apply(graph);

    // Phase II — transformation and constraint enforcement.
    ConstraintPhase {
        graph,
        fixed,
        alignment,
        relative,
    }
    .run();

    // Pre-compile edge indices once before the physics loop.
    let layout_state = LayoutState::new(graph);
    let mut engine = PhysicsEngine::calibrate(graph.nodes.len());

    // Phase III — force-directed polishing with constraint maintenance.
    for _ in 0..max_iter {
        if engine.temperature < 1e-3 {
            break;
        }
        engine.tick(graph, &layout_state, fixed, alignment, relative);
    }
}
