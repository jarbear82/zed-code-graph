use super::math::Vector2;
use hashbrown::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy)]
pub struct Padding {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Padding {
    pub fn uniform(val: f64) -> Self {
        Self {
            top: val,
            right: val,
            bottom: val,
            left: val,
        }
    }
}

/// Reserved base for dummy NodeIds created during spectral preprocessing (§4.1.1).
/// These IDs are local to SpectralLayout and never inserted into CompoundGraph.
pub const DUMMY_ID_BASE: u32 = 0xFFFF_0000;

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub width: f64,
    pub height: f64,
    /// Center of the node's bounding rectangle.
    pub pos: Vector2,
    /// True when this node acts as a compound container.
    pub is_compound: bool,
    /// Child node IDs (non-empty only when is_compound).
    pub children: Vec<NodeId>,
    /// Parent compound node, if any.
    pub parent_id: Option<NodeId>,
}

impl Node {
    pub fn new(id: NodeId, width: f64, height: f64) -> Self {
        Self {
            id,
            width,
            height,
            pos: Vector2::ZERO,
            is_compound: false,
            children: Vec::new(),
            parent_id: None,
        }
    }

    pub fn new_compound(id: NodeId) -> Self {
        Self {
            id,
            width: 0.0,
            height: 0.0,
            pos: Vector2::ZERO,
            is_compound: true,
            children: Vec::new(),
            parent_id: None,
        }
    }

    /// Axis-aligned bounding box: [min_x, min_y, max_x, max_y].
    pub fn aabb(&self) -> [f64; 4] {
        let (hw, hh) = (self.width * 0.5, self.height * 0.5);
        [
            self.pos.x - hw,
            self.pos.y - hh,
            self.pos.x + hw,
            self.pos.y + hh,
        ]
    }
}

// ---------------------------------------------------------------------------
// Edge
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub source: NodeId,
    pub target: NodeId,
    pub weight: f64,
}

impl Edge {
    pub fn new(source: NodeId, target: NodeId) -> Self {
        Self {
            source,
            target,
            weight: 1.0,
        }
    }

    pub fn weighted(source: NodeId, target: NodeId, weight: f64) -> Self {
        Self {
            source,
            target,
            weight,
        }
    }
}

// ---------------------------------------------------------------------------
// CompoundGraph
// ---------------------------------------------------------------------------

pub struct CompoundGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub id_to_idx: HashMap<NodeId, usize>,
    pub degrees: HashMap<NodeId, usize>,
}

impl CompoundGraph {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            id_to_idx: HashMap::new(),
            degrees: HashMap::new(),
        }
    }

    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = node.id;
        let idx = self.nodes.len();
        self.nodes.push(node);
        self.id_to_idx.insert(id, idx);
        id
    }

    pub fn add_edge(&mut self, source: NodeId, target: NodeId) {
        self.edges.push(Edge::new(source, target));
        // Update cached degrees
        *self.degrees.entry(source).or_insert(0) += 1;
        *self.degrees.entry(target).or_insert(0) += 1;
    }

    pub fn add_weighted_edge(&mut self, source: NodeId, target: NodeId, weight: f64) {
        self.edges.push(Edge::weighted(source, target, weight));
    }

    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[self.id_to_idx[&id]]
    }

    #[inline]
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        debug_assert!(
            self.id_to_idx.contains_key(&id),
            "Invariant violation: attempted to access NodeId({}) which does not exist in the graph's ID map.",
            id.0
        );
        let idx = self.id_to_idx[&id];
        &mut self.nodes[idx]
    }

    pub fn remove_edge(&mut self, source: NodeId, target: NodeId) {
        self.edges
            .retain(|e| e.source != source || e.target != target);
        // Update cached degrees (safe decrement)
        if let Some(&deg) = self.degrees.get(&source) {
            *self.degrees.get_mut(&source).unwrap() = deg.saturating_sub(1);
        }
        if let Some(&deg) = self.degrees.get(&target) {
            *self.degrees.get_mut(&target).unwrap() = deg.saturating_sub(1);
        }
    }

    pub fn degree(&self, id: NodeId) -> usize {
        self.edges
            .iter()
            .filter(|e| e.source == id || e.target == id)
            .count()
    }

    /// Refit a compound node's position and size to tightly wrap its children
    /// with `padding` added on all sides (§4.1.3 postprocessing).
    ///
    /// FIX: the center is (min+max)/2 regardless of padding — padding only
    /// widens the bounding box, it does not shift the center.
    // Replace everything from line 147 downwards with exactly this:
    pub fn update_compound_bounds(&mut self, id: NodeId, padding: &Padding) {
        let children: Vec<NodeId> = self.node(id).children.clone();
        if children.is_empty() {
            return;
        }

        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        for cid in children {
            let bb = self.node(cid).aabb();
            min_x = min_x.min(bb[0]);
            min_y = min_y.min(bb[1]);
            max_x = max_x.max(bb[2]);
            max_y = max_y.max(bb[3]);
        }

        let n = self.node_mut(id);

        n.width = (max_x - min_x) + padding.left + padding.right;
        n.height = (max_y - min_y) + padding.top + padding.bottom;

        let content_center_x = (min_x + max_x) / 2.0;
        let content_center_y = (min_y + max_y) / 2.0;

        n.pos.x = content_center_x + (padding.right - padding.left) / 2.0;
        n.pos.y = content_center_y + (padding.bottom - padding.top) / 2.0;
    }
}
