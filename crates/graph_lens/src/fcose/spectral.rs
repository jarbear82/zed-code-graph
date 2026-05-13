//! Phase I — draft layout via Sampled Spectral Distance Embedding (SSDE) with
//! Nyström completion (§4.1.2, Civril et al.).
//!
//! Pipeline:
//!   1. Preprocessing  – flatten compound graph; compound-aware BFS to detect
//!      components; insert dummy nodes to connect them (§4.1.1).
//!   2. SSDE           – k-pivot max-min BFS, k×k CMDS, Nyström extension.
//!      Total cost O(k(n+m)) ≈ O(n+m) for k = O(√n).
//!   3. Postprocessing – strip dummy positions; restore compound bounds (§4.1.3).

use super::graph::{CompoundGraph, DUMMY_ID_BASE, NodeId};
use super::math::Vector2;
use hashbrown::{HashMap, HashSet};
use nalgebra::DMatrix;
use std::collections::VecDeque;

pub struct SpectralLayout;

impl SpectralLayout {
    pub fn apply(graph: &mut CompoundGraph, padding: &super::graph::Padding) {
        let mut dummy_ctr = 0u32;
        let (flat_nodes, flat_edges, dummy_ids) = Self::preprocess(graph, &mut dummy_ctr);
        let positions = Self::ssde(&flat_nodes, &flat_edges);

        for (id, pos) in positions {
            if dummy_ids.contains(&id) {
                continue;
            }
            if let Some(&idx) = graph.id_to_idx.get(&id) {
                graph.nodes[idx].pos = pos;
            }
        }

        Self::restore_compound_positions(graph, padding);
    }

    // -----------------------------------------------------------------------
    // §4.1.1  Preprocessing
    // -----------------------------------------------------------------------

    fn preprocess(
        graph: &CompoundGraph,
        dummy_ctr: &mut u32,
    ) -> (Vec<NodeId>, Vec<(NodeId, NodeId, f64)>, HashSet<NodeId>) {
        let representatives = Self::elect_representatives(graph);

        let mut simple_nodes: Vec<NodeId> = graph
            .nodes
            .iter()
            .filter(|n| !n.is_compound)
            .map(|n| n.id)
            .collect();

        let resolve = |id: NodeId| *representatives.get(&id).unwrap_or(&id);

        let mut simple_edges: Vec<(NodeId, NodeId, f64)> = graph
            .edges
            .iter()
            .filter_map(|e| {
                let s = resolve(e.source);
                let t = resolve(e.target);
                if s == t {
                    return None;
                }
                if simple_nodes.contains(&s) && simple_nodes.contains(&t) {
                    Some((s, t, e.weight))
                } else {
                    None
                }
            })
            .collect();

        let dummy_ids =
            Self::connect_components(graph, &mut simple_nodes, &mut simple_edges, dummy_ctr);

        (simple_nodes, simple_edges, dummy_ids)
    }

    /// For each compound node elect the lowest-degree simple child as its
    /// representative so intra-graph edges can be redirected to a simple node.
    fn elect_representatives(graph: &CompoundGraph) -> HashMap<NodeId, NodeId> {
        let mut map = HashMap::new();
        for node in &graph.nodes {
            if !node.is_compound {
                continue;
            }
            let rep = node
                .children
                .iter()
                .copied()
                .filter(|&c| !graph.node(c).is_compound)
                .min_by_key(|&c| graph.degree(c))
                .or_else(|| node.children.first().copied());
            if let Some(r) = rep {
                map.insert(node.id, r);
            }
        }
        map
    }

    /// Compound-aware BFS component detection then dummy-node stitching.
    ///
    /// The BFS treats parent-child inclusion edges as traversable so that a
    /// compound node and all its descendants are always in the same component
    /// (§4.1.1: "upon reaching a parent compound node, all nodes in its nested
    /// child graph are also reached and vice versa").
    fn connect_components(
        graph: &CompoundGraph,
        nodes: &mut Vec<NodeId>,
        edges: &mut Vec<(NodeId, NodeId, f64)>,
        dummy_ctr: &mut u32,
    ) -> HashSet<NodeId> {
        let mut dummy_ids = HashSet::new();

        // Build undirected adjacency including inclusion (parent↔child) edges.
        let mut adj: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for &(s, t, _) in edges.iter() {
            adj.entry(s).or_default().push(t);
            adj.entry(t).or_default().push(s);
        }
        for n in &graph.nodes {
            if let Some(pid) = n.parent_id {
                adj.entry(n.id).or_default().push(pid);
                adj.entry(pid).or_default().push(n.id);
            }
        }

        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut components: Vec<Vec<NodeId>> = Vec::new();

        for &start in nodes.iter() {
            if visited.contains(&start) {
                continue;
            }
            let mut comp = Vec::new();
            let mut q = VecDeque::new();
            q.push_back(start);
            visited.insert(start);
            while let Some(u) = q.pop_front() {
                if nodes.contains(&u) {
                    comp.push(u);
                }
                for &v in adj.get(&u).unwrap_or(&Vec::new()) {
                    if visited.insert(v) {
                        q.push_back(v);
                    }
                }
            }
            if !comp.is_empty() {
                components.push(comp);
            }
        }

        if components.len() <= 1 {
            return dummy_ids;
        }

        let mut deg: HashMap<NodeId, usize> = HashMap::new();
        for &(s, t, _) in edges.iter() {
            *deg.entry(s).or_insert(0) += 1;
            *deg.entry(t).or_insert(0) += 1;
        }

        // One hub dummy ties all components together; each contributes its
        // minimum-degree node (§4.1.1).
        let dummy = NodeId(DUMMY_ID_BASE + *dummy_ctr);
        *dummy_ctr += 1;
        nodes.push(dummy);
        dummy_ids.insert(dummy);

        for comp in &components {
            let rep = comp
                .iter()
                .copied()
                .min_by_key(|n| *deg.get(n).unwrap_or(&0))
                .unwrap();
            edges.push((rep, dummy, 1e-2));
        }

        dummy_ids
    }

    // -----------------------------------------------------------------------
    // §4.1.2  SSDE — Sampled Spectral Distance Embedding + Nyström completion
    // -----------------------------------------------------------------------

    /// k = ⌈√n⌉, clamped to [2, 50].
    fn k_for_n(n: usize) -> usize {
        ((n as f64).sqrt().ceil() as usize).clamp(2, 50).min(n)
    }

    /// Unweighted BFS; unreachable nodes receive distance n as a fallback.
    fn bfs_distances(adj: &[Vec<usize>], start: usize, n: usize) -> Vec<f64> {
        let mut dist = vec![f64::MAX; n];
        dist[start] = 0.0;
        let mut q = VecDeque::new();
        q.push_back(start);
        while let Some(u) = q.pop_front() {
            for &v in &adj[u] {
                if dist[v] == f64::MAX {
                    dist[v] = dist[u] + 1.0;
                    q.push_back(v);
                }
            }
        }
        for d in &mut dist {
            if *d == f64::MAX {
                *d = n as f64;
            }
        }
        dist
    }

    /// Max-min pivot selection: each successive pivot is the node farthest
    /// from the already-chosen pivot set, guaranteeing good diameter coverage.
    fn select_pivots_maxmin(adj: &[Vec<usize>], n: usize, k: usize) -> Vec<usize> {
        if n == 0 || k == 0 {
            return Vec::new();
        }
        let mut pivots = vec![0usize];
        let mut min_dists = Self::bfs_distances(adj, 0, n);

        while pivots.len() < k {
            let next = min_dists
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i)
                .unwrap_or(0);
            if pivots.contains(&next) {
                break;
            }
            pivots.push(next);
            let new_d = Self::bfs_distances(adj, next, n);
            for (i, &d) in new_d.iter().enumerate() {
                if d < min_dists[i] {
                    min_dists[i] = d;
                }
            }
        }
        pivots
    }

    /// Main SSDE routine with Nyström matrix completion.
    ///
    /// 1. Select k pivots with max-min coverage.
    /// 2. Run k BFS passes → dists[i][j] = d(pivot_i, node_j).
    /// 3. Build k×k squared-distance matrix D_S; double-centre → B_S.
    /// 4. Eigendecompose B_S; take top-2 positive eigenpairs.
    /// 5. Nyström extension to all n nodes.
    fn ssde(nodes: &[NodeId], edges: &[(NodeId, NodeId, f64)]) -> HashMap<NodeId, Vector2> {
        let n = nodes.len();
        if n == 0 {
            return HashMap::new();
        }
        if n == 1 {
            return std::iter::once((nodes[0], Vector2::ZERO)).collect();
        }

        let local: HashMap<NodeId, usize> =
            nodes.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let mut adj = vec![vec![]; n];
        for &(s, t, _) in edges {
            if let (Some(&si), Some(&ti)) = (local.get(&s), local.get(&t)) {
                if si != ti {
                    adj[si].push(ti);
                    adj[ti].push(si);
                }
            }
        }

        let k_req = Self::k_for_n(n);
        let pivots = Self::select_pivots_maxmin(&adj, n, k_req);
        let k = pivots.len();
        if k < 2 {
            return nodes.iter().map(|&id| (id, Vector2::ZERO)).collect();
        }

        let dists: Vec<Vec<f64>> = pivots
            .iter()
            .map(|&p| Self::bfs_distances(&adj, p, n))
            .collect();

        // D_S: k×k squared-distance matrix between pivots.
        let mut d_s = DMatrix::<f64>::zeros(k, k);
        for i in 0..k {
            for j in 0..k {
                d_s[(i, j)] = dists[i][pivots[j]].powi(2);
            }
        }

        let mu_row: Vec<f64> = (0..k).map(|i| d_s.row(i).sum() / k as f64).collect();
        let mu_all: f64 = mu_row.iter().sum::<f64>() / k as f64;

        // Double-centre D_S → B_S.
        let mut b_s = DMatrix::<f64>::zeros(k, k);
        for i in 0..k {
            for j in 0..k {
                b_s[(i, j)] = -0.5 * (d_s[(i, j)] - mu_row[i] - mu_row[j] + mu_all);
            }
        }

        let eig = b_s.symmetric_eigen();
        let mut pairs: Vec<(usize, f64)> = eig.eigenvalues.iter().copied().enumerate().collect();
        pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let (x_col, lam_x) = (pairs[0].0, pairs[0].1.max(0.0));
        let (y_col, lam_y) = if pairs.len() > 1 {
            (pairs[1].0, pairs[1].1.max(0.0))
        } else {
            (pairs[0].0, 0.0)
        };

        let inv_sx = if lam_x > 1e-10 {
            lam_x.sqrt().recip()
        } else {
            0.0
        };
        let inv_sy = if lam_y > 1e-10 {
            lam_y.sqrt().recip()
        } else {
            0.0
        };

        let u_x: Vec<f64> = (0..k).map(|i| eig.eigenvectors[(i, x_col)]).collect();
        let u_y: Vec<f64> = (0..k).map(|i| eig.eigenvectors[(i, y_col)]).collect();

        // Nyström extension to all n nodes.
        nodes
            .iter()
            .enumerate()
            .map(|(j, &id)| {
                let d_sq: Vec<f64> = dists.iter().map(|di| di[j] * di[j]).collect();
                let mu_col_j = d_sq.iter().sum::<f64>() / k as f64;

                let (mut dot_x, mut dot_y) = (0.0_f64, 0.0_f64);
                for i in 0..k {
                    let b = -0.5 * (d_sq[i] - mu_row[i] - mu_col_j + mu_all);
                    dot_x += u_x[i] * b;
                    dot_y += u_y[i] * b;
                }
                (id, Vector2::new(dot_x * inv_sx, dot_y * inv_sy))
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // §4.1.3  Postprocessing
    // -----------------------------------------------------------------------

    fn restore_compound_positions(graph: &mut CompoundGraph, padding: &super::graph::Padding) {
        let mut ids_depths: Vec<(NodeId, usize)> = graph
            .nodes
            .iter()
            .filter(|n| n.is_compound)
            .map(|n| (n.id, Self::nesting_depth(graph, n.id)))
            .collect();
        ids_depths.sort_by(|a, b| b.1.cmp(&a.1));

        for (id, _) in ids_depths {
            graph.update_compound_bounds(id, padding);
        }
    }

    fn nesting_depth(graph: &CompoundGraph, mut id: NodeId) -> usize {
        let mut d = 0;
        while let Some(pid) = graph.node(id).parent_id {
            id = pid;
            d += 1;
        }
        d
    }
}
