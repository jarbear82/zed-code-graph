//! Phase II — transformation and constraint enforcement (§4.2, Algorithm 2).

use super::graph::{CompoundGraph, NodeId};
use super::math::Vector2;
use hashbrown::{HashMap, HashSet};
use nalgebra::Matrix2;
use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// Constraint types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FixedConstraint {
    pub id: NodeId,
    pub pos: Vector2,
}

/// `horizontal = true`  → "a – b – c" (shared Y coordinate).
/// `horizontal = false` → "a ∥ b ∥ c" (shared X coordinate).
#[derive(Debug, Clone)]
pub struct AlignmentConstraint {
    pub nodes: Vec<NodeId>,
    pub horizontal: bool,
}

/// `horizontal = true`  → left must be to the LEFT  of right (x-axis gap).
/// `horizontal = false` → left must be ABOVE right          (y-axis gap).
#[derive(Debug, Clone)]
pub struct RelativeConstraint {
    pub left: NodeId,
    pub right: NodeId,
    pub gap: f64,
    pub horizontal: bool,
}

// ---------------------------------------------------------------------------
// ConstraintPhase
// ---------------------------------------------------------------------------

pub struct ConstraintPhase<'g> {
    pub graph: &'g mut CompoundGraph,
    pub fixed: &'g [FixedConstraint],
    pub alignment: &'g [AlignmentConstraint],
    pub relative: &'g [RelativeConstraint],
}

impl<'g> ConstraintPhase<'g> {
    pub fn run(mut self) {
        self.apply_transformation();
        self.enforce_fixed();
        self.enforce_alignment();
        self.enforce_relative();
    }

    // -----------------------------------------------------------------------
    // §4.2.1  Transformation of draft layout
    // -----------------------------------------------------------------------

    /// Three mutually exclusive cases from §4.2.1:
    ///
    /// **Case 1** |C_f| > 1 → Procrustes on fixed-node positions.
    /// **Case 2** |C_f| ≤ 1 and |C_a| > 0 → Procrustes on alignment centroids.
    /// **Case 3** only |C_r| > 0 → DAG-based Procrustes via `calc_xform_relative`.
    ///
    /// Cases 1 and 2 additionally apply majority-reflection for any relative
    /// constraints (§4.2.1 bullet 3).
    fn apply_transformation(&mut self) {
        if self.fixed.len() > 1 {
            let src: Vec<_> = self
                .fixed
                .iter()
                .map(|fc| self.graph.node(fc.id).pos)
                .collect();
            let tgt: Vec<_> = self.fixed.iter().map(|fc| fc.pos).collect();
            if let Some(t) = Self::procrustes(&src, &tgt) {
                Self::apply_matrix(self.graph, &t);
            }
            if !self.relative.is_empty() {
                Self::majority_reflect(self.graph, self.relative);
            }
        } else if !self.alignment.is_empty() {
            let (mut src, mut tgt) = (Vec::new(), Vec::new());
            for ac in self.alignment {
                // For horizontal alignment (shared Y), axis = 1; vertical (shared X), axis = 0.
                let axis = if ac.horizontal { 1usize } else { 0 };
                let avg = ac
                    .nodes
                    .iter()
                    .map(|&id| self.graph.node(id).pos.coord(axis))
                    .sum::<f64>()
                    / ac.nodes.len() as f64;
                for &id in &ac.nodes {
                    let p = self.graph.node(id).pos;
                    src.push(p);
                    tgt.push(if ac.horizontal {
                        Vector2::new(p.x, avg)
                    } else {
                        Vector2::new(avg, p.y)
                    });
                }
            }
            if let Some(t) = Self::procrustes(&src, &tgt) {
                Self::apply_matrix(self.graph, &t);
            }
            if !self.relative.is_empty() {
                Self::majority_reflect(self.graph, self.relative);
            }
        } else if !self.relative.is_empty() {
            self.calc_xform_relative();
        }
    }

    /// Orthogonal Procrustes solution (§4.2.1 / §20 of Borg & Groenen).
    ///
    /// Given source positions `src` and target positions `tgt`, returns the
    /// 2×2 orthogonal matrix T = V Uᵀ where A^T B = U Σ Vᵀ (SVD).
    /// Returns `None` when fewer than 2 point pairs are provided.
    fn procrustes(src: &[Vector2], tgt: &[Vector2]) -> Option<[[f64; 2]; 2]> {
        let n = src.len();
        if n < 2 {
            return None;
        }

        let s_cx = src.iter().map(|v| v.x).sum::<f64>() / n as f64;
        let s_cy = src.iter().map(|v| v.y).sum::<f64>() / n as f64;
        let t_cx = tgt.iter().map(|v| v.x).sum::<f64>() / n as f64;
        let t_cy = tgt.iter().map(|v| v.y).sum::<f64>() / n as f64;

        // Build A^T B where A = centred target, B = centred source.
        let mut ab = [[0f64; 2]; 2];
        for i in 0..n {
            let a = [tgt[i].x - t_cx, tgt[i].y - t_cy];
            let b = [src[i].x - s_cx, src[i].y - s_cy];
            for r in 0..2 {
                for c in 0..2 {
                    ab[r][c] += a[r] * b[c];
                }
            }
        }

        let m = Matrix2::new(ab[0][0], ab[0][1], ab[1][0], ab[1][1]);
        let svd = m.svd(true, true);
        let u = svd.u?;
        let vt = svd.v_t?;
        // T = V Uᵀ  (nalgebra: vt = Vᵀ, so V = vt.transpose())
        let t = vt.transpose() * u.transpose();
        Some([[t[(0, 0)], t[(0, 1)]], [t[(1, 0)], t[(1, 1)]]])
    }

    /// Apply a 2×2 rotation/reflection matrix to every node, rotating about
    /// the layout centroid so the centroid is preserved.
    ///
    /// FIX: the original code rotated about the global origin, shifting the
    /// centroid whenever it was not at (0,0).
    fn apply_matrix(graph: &mut CompoundGraph, t: &[[f64; 2]; 2]) {
        let n = graph.nodes.len();
        if n == 0 {
            return;
        }
        let cx = graph.nodes.iter().map(|nd| nd.pos.x).sum::<f64>() / n as f64;
        let cy = graph.nodes.iter().map(|nd| nd.pos.y).sum::<f64>() / n as f64;
        for nd in &mut graph.nodes {
            let x = nd.pos.x - cx;
            let y = nd.pos.y - cy;
            nd.pos = Vector2::new(
                x * t[0][0] + y * t[0][1] + cx,
                x * t[1][0] + y * t[1][1] + cy,
            );
        }
    }

    /// CalcXFormRelative (§4.2.1, last bullet): orient the draft layout using
    /// the topology of the constraint DAGs via longest-path Procrustes.
    fn calc_xform_relative(&mut self) {
        let mut dh_nodes = HashSet::new();
        let mut dv_nodes = HashSet::new();
        let mut dh_edges = Vec::new();
        let mut dv_edges = Vec::new();

        for rc in self.relative {
            if rc.horizontal {
                dh_nodes.insert(rc.left);
                dh_nodes.insert(rc.right);
                dh_edges.push((rc.left, rc.right, rc.gap));
            } else {
                dv_nodes.insert(rc.left);
                dv_nodes.insert(rc.right);
                dv_edges.push((rc.left, rc.right, rc.gap));
            }
        }

        let all_dag_nodes: Vec<NodeId> = dh_nodes
            .iter()
            .chain(dv_nodes.iter())
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let total = all_dag_nodes.len();
        if total == 0 {
            return;
        }

        let all_edges: Vec<(NodeId, NodeId, f64)> =
            dh_edges.iter().chain(dv_edges.iter()).copied().collect();
        let components = Self::weakly_connected_components(&all_dag_nodes, &all_edges);

        let largest = match components.iter().max_by_key(|c| c.len()) {
            Some(c) => c.clone(),
            None => return,
        };

        if largest.len() * 2 < total {
            Self::majority_reflect(self.graph, self.relative);
            return;
        }

        let largest_set: HashSet<NodeId> = largest.iter().copied().collect();
        let vih: Vec<NodeId> = dh_nodes
            .iter()
            .filter(|n| largest_set.contains(*n))
            .copied()
            .collect();
        let viv: Vec<NodeId> = dv_nodes
            .iter()
            .filter(|n| largest_set.contains(*n))
            .copied()
            .collect();

        let x_targets = Self::longest_path_distances(&dh_edges, &vih);
        let y_targets = Self::longest_path_distances(&dv_edges, &viv);

        let mut src = Vec::with_capacity(largest.len());
        let mut tgt = Vec::with_capacity(largest.len());

        for &node in &largest {
            let curr = self.graph.node(node).pos;
            let tx = x_targets.get(&node).copied().unwrap_or(curr.x);
            let ty = y_targets.get(&node).copied().unwrap_or(curr.y);
            src.push(curr);
            tgt.push(Vector2::new(tx, ty));
        }

        if src.len() >= 2 {
            if let Some(t) = Self::procrustes(&src, &tgt) {
                Self::apply_matrix(self.graph, &t);
            }
        }

        // Additional majority-reflection to clean up any residual directional
        // bias after the Procrustes step.
        Self::majority_reflect(self.graph, self.relative);
    }

    /// Reflect the graph on the y-axis (for horizontal constraints) or x-axis
    /// (for vertical constraints) when the majority of constraints are violated.
    ///
    /// FIX: uses `satisfied * 2 < n` instead of `satisfied < n / 2` to
    /// correctly handle odd constraint counts (integer division truncates).
    fn majority_reflect(graph: &mut CompoundGraph, rel: &[RelativeConstraint]) {
        for horiz in [true, false] {
            let constraints: Vec<_> = rel.iter().filter(|r| r.horizontal == horiz).collect();
            if constraints.is_empty() {
                continue;
            }
            let axis = if horiz { 0usize } else { 1 };
            let n = constraints.len();
            let satisfied = constraints
                .iter()
                .filter(|r| {
                    graph.node(r.left).pos.coord(axis) + r.gap
                        <= graph.node(r.right).pos.coord(axis)
                })
                .count();
            // Reflect if strictly more than half are violated.
            if satisfied * 2 < n {
                for nd in &mut graph.nodes {
                    if horiz {
                        nd.pos.x = -nd.pos.x;
                    } else {
                        nd.pos.y = -nd.pos.y;
                    }
                }
            }
        }
    }

    /// Longest-path distances from all in-degree-zero nodes in the sub-DAG
    /// induced by `nodes`. Used to build target coordinates for Procrustes.
    fn longest_path_distances(
        edges: &[(NodeId, NodeId, f64)],
        nodes: &[NodeId],
    ) -> HashMap<NodeId, f64> {
        if nodes.is_empty() {
            return HashMap::new();
        }
        let node_set: HashSet<NodeId> = nodes.iter().copied().collect();

        let mut adj: HashMap<NodeId, Vec<(NodeId, f64)>> =
            nodes.iter().map(|&n| (n, Vec::new())).collect();
        let mut in_deg: HashMap<NodeId, usize> = nodes.iter().map(|&n| (n, 0usize)).collect();

        for &(s, t, w) in edges {
            if node_set.contains(&s) && node_set.contains(&t) {
                adj.entry(s).or_default().push((t, w));
                *in_deg.entry(t).or_insert(0) += 1;
            }
        }

        let mut dist: HashMap<NodeId, f64> =
            nodes.iter().map(|&n| (n, f64::NEG_INFINITY)).collect();
        let mut work = in_deg.clone();
        let mut queue: VecDeque<NodeId> = VecDeque::new();

        for &n in nodes {
            if in_deg[&n] == 0 {
                dist.insert(n, 0.0);
                queue.push_back(n);
            }
        }

        while let Some(u) = queue.pop_front() {
            let u_dist = dist[&u];
            if let Some(neighbors) = adj.get(&u).cloned() {
                for (v, w) in neighbors {
                    let cand = u_dist + w;
                    let entry = dist.entry(v).or_insert(f64::NEG_INFINITY);
                    if cand > *entry {
                        *entry = cand;
                    }
                    let deg = work.entry(v).or_insert(0);
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        queue.push_back(v);
                    }
                }
            }
        }
        dist
    }

    fn weakly_connected_components(
        nodes: &[NodeId],
        edges: &[(NodeId, NodeId, f64)],
    ) -> Vec<Vec<NodeId>> {
        let mut undir: HashMap<NodeId, Vec<NodeId>> =
            nodes.iter().map(|&n| (n, Vec::new())).collect();
        for &(s, t, _) in edges {
            undir.entry(s).or_default().push(t);
            undir.entry(t).or_default().push(s);
        }
        let mut visited = HashSet::new();
        let mut components = Vec::new();
        for &start in nodes {
            if visited.contains(&start) {
                continue;
            }
            let mut comp = Vec::new();
            let mut q = VecDeque::new();
            q.push_back(start);
            visited.insert(start);
            while let Some(u) = q.pop_front() {
                comp.push(u);
                for &v in undir.get(&u).unwrap_or(&Vec::new()) {
                    if visited.insert(v) {
                        q.push_back(v);
                    }
                }
            }
            components.push(comp);
        }
        components
    }

    // -----------------------------------------------------------------------
    // §4.2.2  Enforcing constraints
    // -----------------------------------------------------------------------

    /// §4.2.2 fixed-node enforcement.
    ///
    /// Each fixed node is snapped to its target position.  The rest of the
    /// graph is translated by the average displacement so the layout drifts
    /// minimally (paper §4.2.2, eq. δ_x / δ_y).
    fn enforce_fixed(&mut self) {
        if self.fixed.is_empty() {
            return;
        }
        let dx = self
            .fixed
            .iter()
            .map(|fc| fc.pos.x - self.graph.node(fc.id).pos.x)
            .sum::<f64>()
            / self.fixed.len() as f64;
        let dy = self
            .fixed
            .iter()
            .map(|fc| fc.pos.y - self.graph.node(fc.id).pos.y)
            .sum::<f64>()
            / self.fixed.len() as f64;

        let fixed_set: HashSet<NodeId> = self.fixed.iter().map(|fc| fc.id).collect();
        for n in &mut self.graph.nodes {
            if !fixed_set.contains(&n.id) {
                n.pos.x += dx;
                n.pos.y += dy;
            }
        }
        for fc in self.fixed {
            self.graph.node_mut(fc.id).pos = fc.pos;
        }
    }

    /// §4.2.2 alignment enforcement.
    ///
    /// All nodes in a constraint are moved to their average coordinate in
    /// the constrained axis.
    fn enforce_alignment(&mut self) {
        for ac in self.alignment {
            // horizontal=true → shared Y (axis 1); horizontal=false → shared X (axis 0).
            let axis = if ac.horizontal { 1usize } else { 0 };
            let avg = ac
                .nodes
                .iter()
                .map(|&id| self.graph.node(id).pos.coord(axis))
                .sum::<f64>()
                / ac.nodes.len() as f64;
            for &id in &ac.nodes {
                self.graph.node_mut(id).pos.set_coord(axis, avg);
            }
        }
    }

    fn enforce_relative(&mut self) {
        if self.relative.is_empty() {
            return;
        }
        let fixed_set: HashSet<NodeId> = self.fixed.iter().map(|fc| fc.id).collect();
        for (horiz, axis) in [(true, 0usize), (false, 1usize)] {
            self.enforce_relative_dir(horiz, axis, &fixed_set);
        }
    }

    /// Algorithm 2: enforce relative placement constraints for one axis.
    ///
    /// Key design decisions vs. paper pseudocode:
    ///
    /// **Meta-nodes** — alignment constraints in the *perpendicular* direction
    ///   are collapsed into synthetic meta-nodes so that aligned nodes move as
    ///   a rigid group along this axis.
    ///
    /// **curr_pos initialisation** — meta-node IDs do not exist in the graph,
    ///   so the initial position map is built only for real nodes; meta-node
    ///   positions are then inserted separately as the average of their members.
    ///   (FIX: the original code called `graph.node(meta_id)` and panicked.)
    ///
    /// **pred_list** — accumulated as a HashSet union over the full predecessor
    ///   chain so that fixed-node violations are back-propagated through every
    ///   branch of a diamond-shaped DAG.
    ///
    /// **Adjacency** — a prebuilt HashMap replaces the original O(E) linear
    ///   scan per neighbour query, restoring the O(V+E) complexity of Alg. 2.
    fn enforce_relative_dir(&mut self, horiz: bool, axis: usize, fixed_set: &HashSet<NodeId>) {
        const META_BASE: u32 = 0xEEEE_0000;
        let mut meta_ctr = 0u32;
        let mut node_to_meta: HashMap<NodeId, NodeId> = HashMap::new();
        let mut meta_members: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        // Create one meta-node per alignment group in the opposite direction.
        for ac in self.alignment.iter().filter(|ac| ac.horizontal != horiz) {
            let mid = NodeId(META_BASE + meta_ctr);
            meta_ctr += 1;
            for &nid in &ac.nodes {
                node_to_meta.insert(nid, mid);
            }
            meta_members.insert(mid, ac.nodes.clone());
        }

        // A meta-node is "fixed" if any of its members is fixed.
        let fixed_metas: HashSet<NodeId> = meta_members
            .iter()
            .filter(|(_, ms)| ms.iter().any(|n| fixed_set.contains(n)))
            .map(|(&mid, _)| mid)
            .collect();

        let resolve = |id: NodeId| *node_to_meta.get(&id).unwrap_or(&id);

        let rel_dir: Vec<(NodeId, NodeId, f64)> = self
            .relative
            .iter()
            .filter(|r| r.horizontal == horiz)
            .map(|r| (resolve(r.left), resolve(r.right), r.gap))
            .filter(|(s, t, _)| s != t)
            .collect();

        if rel_dir.is_empty() {
            return;
        }

        // Prebuilt adjacency map: O(E) construction, O(1) neighbour lookup.
        let mut dag_adj: HashMap<NodeId, Vec<(NodeId, f64)>> = HashMap::new();
        for &(s, t, w) in &rel_dir {
            dag_adj.entry(s).or_default().push((t, w));
        }

        let mut dag_nodes: HashSet<NodeId> = HashSet::new();
        for &(s, t, _) in &rel_dir {
            dag_nodes.insert(s);
            dag_nodes.insert(t);
        }
        let dag_node_list: Vec<NodeId> = dag_nodes.iter().copied().collect();

        // FIX: skip meta-node IDs when looking up positions in the graph —
        // they do not exist as graph nodes.  Meta positions are computed
        // separately below.
        let meta_ids: HashSet<NodeId> = meta_members.keys().copied().collect();
        let mut curr_pos: HashMap<NodeId, f64> = dag_node_list
            .iter()
            .copied()
            .filter(|n| !meta_ids.contains(n))
            .map(|n| (n, self.graph.node(n).pos.coord(axis)))
            .collect();

        for (&mid, members) in &meta_members {
            if dag_nodes.contains(&mid) {
                let sum: f64 = members
                    .iter()
                    .map(|&n| self.graph.node(n).pos.coord(axis))
                    .sum();
                curr_pos.insert(mid, sum / members.len() as f64);
            }
        }

        let components = Self::weakly_connected_components(&dag_node_list, &rel_dir);

        let is_fixed =
            |id: NodeId| -> bool { fixed_set.contains(&id) || fixed_metas.contains(&id) };

        let mut new_pos: HashMap<NodeId, f64> = HashMap::new();

        for component in &components {
            let comp_set: HashSet<NodeId> = component.iter().copied().collect();

            // In-degree computation using the prebuilt adjacency map.
            let mut in_deg: HashMap<NodeId, usize> =
                component.iter().map(|&n| (n, 0usize)).collect();
            for &n in component {
                if let Some(nbrs) = dag_adj.get(&n) {
                    for &(v, _) in nbrs {
                        if comp_set.contains(&v) {
                            *in_deg.entry(v).or_insert(0) += 1;
                        }
                    }
                }
            }

            let mut pred_list: HashMap<NodeId, HashSet<NodeId>> = component
                .iter()
                .map(|&n| {
                    let mut s = HashSet::new();
                    s.insert(n);
                    (n, s)
                })
                .collect();

            let mut work_in_deg = in_deg.clone();
            let mut queue: VecDeque<NodeId> = VecDeque::new();

            for &n in component {
                if in_deg[&n] == 0 {
                    new_pos.insert(n, curr_pos.get(&n).copied().unwrap_or(0.0));
                    queue.push_back(n);
                } else {
                    new_pos.insert(n, f64::NEG_INFINITY);
                }
            }

            while let Some(u) = queue.pop_front() {
                let u_pos = new_pos[&u];
                let u_preds: HashSet<NodeId> = pred_list[&u].clone();

                // Collect neighbours from prebuilt map, filtered to this component.
                let neighbors: Vec<(NodeId, f64)> = dag_adj
                    .get(&u)
                    .map(|nbrs| {
                        nbrs.iter()
                            .filter(|(v, _)| comp_set.contains(v))
                            .copied()
                            .collect()
                    })
                    .unwrap_or_default();

                for (v, gap) in neighbors {
                    let required = u_pos + gap;
                    let v_cur = curr_pos.get(&v).copied().unwrap_or(0.0);

                    if required > new_pos[&v] {
                        if is_fixed(v) {
                            // Pin v at its current position.
                            new_pos.insert(v, v_cur);
                            if v_cur < required {
                                // Constraint still violated: pull u's entire
                                // predecessor chain back by the discrepancy.
                                let discr = required - v_cur;
                                for &w in &u_preds {
                                    if let Some(wp) = new_pos.get_mut(&w) {
                                        *wp -= discr;
                                    }
                                }
                            }
                        } else {
                            new_pos.insert(v, required);
                        }
                    }

                    // Inherit the full predecessor chain of u (including u).
                    let mut full_chain = u_preds.clone();
                    full_chain.insert(u);
                    pred_list.entry(v).or_default().extend(full_chain);

                    let deg = work_in_deg.entry(v).or_insert(0);
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        queue.push_back(v);
                    }
                }
            }
        }

        // Write computed positions back to the graph.
        for (dag_node, pos) in new_pos {
            if let Some(members) = meta_members.get(&dag_node) {
                // A meta-node represents all members of the alignment group.
                for &m in members {
                    if let Some(&idx) = self.graph.id_to_idx.get(&m) {
                        self.graph.nodes[idx].pos.set_coord(axis, pos);
                    }
                }
            } else if let Some(&idx) = self.graph.id_to_idx.get(&dag_node) {
                self.graph.nodes[idx].pos.set_coord(axis, pos);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::math::Vector2;
    use super::*;

    #[test]
    fn test_procrustes_identity() {
        let src = vec![Vector2::new(1.0, 1.0), Vector2::new(2.0, 2.0)];
        let tgt = vec![Vector2::new(1.0, 1.0), Vector2::new(2.0, 2.0)];
        let result = ConstraintPhase::procrustes(&src, &tgt);
        assert!(result.is_some());
        let t = result.unwrap();
        // Should return an identity-like transformation (rotation ~ 0)
        assert!((t[0][0] - 1.0).abs() < 1e-4);
        assert!((t[0][1] - 0.0).abs() < 1e-4);
        assert!((t[1][0] - 0.0).abs() < 1e-4);
        assert!((t[1][1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_longest_path_distances() {
        let edges = vec![(NodeId(0), NodeId(1), 5.0), (NodeId(1), NodeId(2), 3.0)];
        let nodes = vec![NodeId(0), NodeId(1), NodeId(2)];
        let dists = ConstraintPhase::longest_path_distances(&edges, &nodes);
        assert!((dists[&NodeId(0)] - 0.0).abs() < 1e-4);
        assert!((dists[&NodeId(1)] - 5.0).abs() < 1e-4);
        assert!((dists[&NodeId(2)] - 8.0).abs() < 1e-4);
    }
}
