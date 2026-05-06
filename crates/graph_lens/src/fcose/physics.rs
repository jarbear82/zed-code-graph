//! Phase III — modified CoSE polishing phase (§4.3).
//!
//! Applies an incremental force-directed simulation starting from the
//! constrained draft layout produced by Phase II.  Key modifications vs.
//! vanilla CoSE:
//!
//!   • AdjustDisplacements — clamp each node's movement so no established
//!     constraint is violated after the tick.
//!   • Compound confinement — a restoring force keeps children inside their
//!     parent's current bounding box.
//!   • R-Tree repulsion — O(N log N) spatial query replaces the O(N²) naive
//!     all-pairs loop.
//!   • Calibrated cooling — the cooling rate scales with graph size.

use super::constraints::{AlignmentConstraint, FixedConstraint, RelativeConstraint};
use super::graph::{CompoundGraph, NodeId};
use super::math::Vector2;
use rstar::{AABB, RTree, primitives::GeomWithData};

type SpatialEntry = GeomWithData<[f64; 2], usize>;

// ---------------------------------------------------------------------------
// LayoutState — pre-compiled index buffer
// ---------------------------------------------------------------------------

/// Strips per-tick HashMap lookups out of the physics loop by caching raw
/// `usize` indices into `graph.nodes`.
pub struct LayoutState {
    pub edges: Vec<(usize, usize, f64)>,
}

impl LayoutState {
    pub fn new(graph: &CompoundGraph) -> Self {
        let edges = graph
            .edges
            .iter()
            .filter_map(|e| {
                let s = *graph.id_to_idx.get(&e.source)?;
                let t = *graph.id_to_idx.get(&e.target)?;
                Some((s, t, e.weight))
            })
            .collect();
        Self { edges }
    }
}

// ---------------------------------------------------------------------------
// PhysicsEngine
// ---------------------------------------------------------------------------

pub struct PhysicsEngine {
    pub temperature: f64,
    pub cooling_rate: f64,
    pub ideal_edge_len: f64,
    /// k² in Fruchterman-Reingold repulsion: f_r = k² / dist.
    pub repulsion_k_sq: f64,
    pub spring_k: f64,
    pub compound_padding: f64,
}

impl Default for PhysicsEngine {
    fn default() -> Self {
        Self {
            temperature: 0.1,
            cooling_rate: 0.95,
            ideal_edge_len: 50.0,
            repulsion_k_sq: 4500.0,
            spring_k: 0.45,
            compound_padding: 10.0,
        }
    }
}

impl PhysicsEngine {
    /// Choose a cooling rate appropriate for the graph size.
    /// Larger graphs need a slower decay to resolve complex overlaps.
    pub fn calibrate(num_nodes: usize) -> Self {
        let mut engine = Self::default();
        engine.cooling_rate = if num_nodes > 1000 {
            0.99
        } else if num_nodes > 250 {
            0.98
        } else {
            0.95
        };
        engine
    }

    /// One simulation tick — matches the inner loop of Algorithm 1.
    pub fn tick(
        &mut self,
        graph: &mut CompoundGraph,
        state: &LayoutState,
        fixed: &[FixedConstraint],
        alignment: &[AlignmentConstraint],
        relative: &[RelativeConstraint],
    ) {
        let n = graph.nodes.len();
        if n == 0 || self.temperature < 1e-4 {
            return;
        }

        // ── 1. Build R-Tree for O(N log N) repulsion ──
        // Compound nodes are excluded: their bounds are updated via
        // UpdateBounds, not by direct force application.
        let entries: Vec<SpatialEntry> = graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, nd)| !nd.is_compound)
            .map(|(i, nd)| GeomWithData::new([nd.pos.x, nd.pos.y], i))
            .collect();
        let tree = RTree::bulk_load(entries);

        // ── 2. Accumulate forces ──
        let mut forces = vec![Vector2::ZERO; n];
        let search_r = self.ideal_edge_len * 2.0;

        for i in 0..n {
            if graph.nodes[i].is_compound {
                continue;
            }

            let p = graph.nodes[i].pos;
            let hw = graph.nodes[i].width * 0.5;
            let hh = graph.nodes[i].height * 0.5;

            // Expand the envelope by the node's dimensions so we don't miss
            // large overlapping nodes whose centers are far away.
            let env = AABB::from_corners(
                [p.x - hw - search_r, p.y - hh - search_r],
                [p.x + hw + search_r, p.y + hh + search_r],
            );

            for entry in tree.locate_in_envelope(&env) {
                let j = entry.data;
                if i == j {
                    continue;
                }
                // Pass the full node references to account for bounding boxes
                forces[i] += self.repulsion(&graph.nodes[i], &graph.nodes[j]);
            }

            // Restoring force: keep node inside its parent compound's bounds.
            if let Some(pid) = graph.nodes[i].parent_id {
                let bb = graph.node(pid).aabb();
                let pen_x = f64::max(0.0, bb[0] - (p.x - hw)) - f64::max(0.0, (p.x + hw) - bb[2]);
                let pen_y = f64::max(0.0, bb[1] - (p.y - hh)) - f64::max(0.0, (p.y + hh) - bb[3]);
                forces[i] += Vector2::new(pen_x, pen_y) * 0.5;
            }
        }

        // Spring attraction along graph edges (pre-compiled indices).
        for &(ui, vi, _weight) in &state.edges {
            let f = self.spring(graph.nodes[ui].pos, graph.nodes[vi].pos);
            forces[ui] += f;
            forces[vi] -= f;
        }

        // ── 3. Scale by temperature → candidate displacements ──
        let mut disp: Vec<Vector2> = forces.iter().map(|&f| f * self.temperature).collect();

        // ── 4. AdjustDisplacements (§4.3) ──
        Self::adjust_displacements(graph, &mut disp, fixed, alignment, relative);

        // ── 5. Move nodes ──
        for i in 0..n {
            if graph.nodes[i].is_compound {
                continue;
            }
            graph.nodes[i].pos += disp[i];
        }

        // ── 6. UpdateBounds — innermost compounds first ──
        let mut compound_depths: Vec<(NodeId, usize)> = graph
            .nodes
            .iter()
            .filter(|nd| nd.is_compound)
            .map(|nd| {
                let mut d = 0;
                let mut cur = nd.parent_id;
                while let Some(p) = cur {
                    cur = graph.node(p).parent_id;
                    d += 1;
                }
                (nd.id, d)
            })
            .collect();
        compound_depths.sort_by(|a, b| b.1.cmp(&a.1));

        let padding = self.compound_padding;
        for (id, _) in compound_depths {
            graph.update_compound_bounds(id, padding);
        }

        // ── 7. Cool ──
        self.temperature *= self.cooling_rate;
    }

    /// Fruchterman-Reingold repulsion: f_r = k² / dist (§4.3, [13]).
    ///
    /// F-R specifies k² / dist which corresponds to
    ///   force_vector = (d / |d|) * k² / |d| = d * k² / |d|² = d * k² / dist_sq.
    /// Modified Fruchterman-Reingold repulsion using AABB overlap resolution.
    #[inline]
    fn repulsion(&self, node_a: &super::graph::Node, node_b: &super::graph::Node) -> Vector2 {
        let d = node_a.pos - node_b.pos;

        let min_dist_x = (node_a.width + node_b.width) * 0.5;
        let min_dist_y = (node_a.height + node_b.height) * 0.5;

        // Gap between the edges of the rectangles (negative means overlapping)
        let gap_x = d.x.abs() - min_dist_x;
        let gap_y = d.y.abs() - min_dist_y;

        // Handle physical overlap (Collision Resolution)
        if gap_x < 0.0 && gap_y < 0.0 {
            // Apply a strong penalty force proportional to the penetration depth
            let overlap_force_multiplier = 5.0;

            if gap_x > gap_y {
                // Penetrating less on the X axis, push horizontally
                let sign = if d.x > 0.0 { 1.0 } else { -1.0 };
                return Vector2::new(sign * gap_x.abs() * overlap_force_multiplier, 0.0);
            } else {
                // Penetrating less on the Y axis, push vertically
                let sign = if d.y > 0.0 { 1.0 } else { -1.0 };
                return Vector2::new(0.0, sign * gap_y.abs() * overlap_force_multiplier);
            }
        }

        // Standard layout repulsion for nodes that are NOT overlapping
        let effective_dist = gap_x.max(gap_y).max(1.0);
        let dist_sq = effective_dist * effective_dist;

        d.normalize() * (self.repulsion_k_sq / dist_sq)
    }

    #[inline]
    fn spring(&self, u: Vector2, v: Vector2) -> Vector2 {
        let d = v - u;
        let dist = d.length().max(1.0);
        let mag = self.spring_k * (dist - self.ideal_edge_len);
        d.normalize() * mag
    }

    /// Clamp displacement vectors before applying them so that no established
    /// constraint is violated after the tick (§4.3 AdjustDisplacements).
    fn adjust_displacements(
        graph: &CompoundGraph,
        disp: &mut Vec<Vector2>,
        fixed: &[FixedConstraint],
        alignment: &[AlignmentConstraint],
        relative: &[RelativeConstraint],
    ) {
        // Fixed nodes do not move.
        for fc in fixed {
            if let Some(&idx) = graph.id_to_idx.get(&fc.id) {
                disp[idx] = Vector2::ZERO;
            }
        }

        // Aligned nodes must receive the same displacement along the shared axis.
        for ac in alignment {
            let axis = if ac.horizontal { 1usize } else { 0 };
            let avg_d = ac
                .nodes
                .iter()
                .filter_map(|id| graph.id_to_idx.get(id))
                .map(|&idx| disp[idx].coord(axis))
                .sum::<f64>()
                / ac.nodes.len().max(1) as f64;
            for &id in &ac.nodes {
                if let Some(&idx) = graph.id_to_idx.get(&id) {
                    disp[idx].set_coord(axis, avg_d);
                }
            }
        }

        // Relative constraints: redistribute excess movement symmetrically.
        for rc in relative {
            let (Some(&li), Some(&ri)) = (
                graph.id_to_idx.get(&rc.left),
                graph.id_to_idx.get(&rc.right),
            ) else {
                continue;
            };
            let axis = if rc.horizontal { 0usize } else { 1 };
            let left_new = graph.nodes[li].pos.coord(axis) + disp[li].coord(axis);
            let right_new = graph.nodes[ri].pos.coord(axis) + disp[ri].coord(axis);

            if left_new + rc.gap > right_new {
                let excess = (left_new + rc.gap - right_new) * 0.5;
                let new_l = disp[li].coord(axis) - excess;
                let new_r = disp[ri].coord(axis) + excess;
                disp[li].set_coord(axis, new_l);
                disp[ri].set_coord(axis, new_r);
            }
        }
    }
}
