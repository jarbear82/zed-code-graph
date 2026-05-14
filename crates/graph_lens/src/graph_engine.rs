// src/graph_engine.rs

use gpui::{Point, Size};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EdgeId(pub usize);

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Worktree,
    Directory,
    File,
    Outline,
}

#[derive(Clone)]
pub struct GraphNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,

    // Hierarchy
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub expanded: bool,

    // Physics & Layout
    pub position: Point<f32>,
    pub velocity: Point<f32>,
    pub mass: f32,
    pub external_force: Point<f32>,

    // NEW: Spatial dimensions for overlap correction and compound directory bounding boxes
    pub size: Size<f32>,

    // NEW: Flag to freeze the node's physics (e.g., when the user is dragging it)
    pub is_fixed: bool,
}

#[derive(Clone)]
pub struct GraphEdge {
    pub id: EdgeId,
    pub source: NodeId,
    pub target: NodeId,
}

pub struct GraphEngine {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Cache of nodes currently participating in physics (visible)
    pub active_nodes: Vec<NodeId>,
    /// Width and Height of the simulation bounds
    pub bounds: Point<f32>,
}

impl GraphEngine {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            active_nodes: Vec::new(),
            bounds: Point {
                x: width,
                y: height,
            },
        }
    }

    pub fn add_node(&mut self, label: String, kind: NodeKind, parent: Option<NodeId>) -> NodeId {
        let id = NodeId(self.nodes.len());

        // NEW: Assign a default starting size based on the node kind.
        // The layout engine will dynamically expand directories later to fit their children.
        let initial_size = match kind {
            NodeKind::Worktree | NodeKind::Directory => Size {
                width: 200.0,
                height: 100.0,
            },
            NodeKind::File | NodeKind::Outline => Size {
                width: 160.0,
                height: 40.0,
            },
        };

        let mut node = GraphNode {
            id,
            kind,
            label,
            parent,
            children: Vec::new(),
            expanded: true,
            position: Point {
                x: (rand::random::<f32>() - 0.5) * 100.0 + (self.bounds.x / 2.0),
                y: (rand::random::<f32>() - 0.5) * 100.0 + (self.bounds.y / 2.0),
            },
            size: initial_size,
            velocity: Point::default(),
            mass: 1.0,
            external_force: Point::default(),
            is_fixed: false,
        };

        if let Some(p_id) = parent {
            self.nodes[p_id.0].children.push(id);
            // Inherit position from parent with a small offset
            node.position = self.nodes[p_id.0].position + Point { x: 5.0, y: 5.0 };
        }

        self.nodes.push(node);
        self.recompute_visibility();
        id
    }

    pub fn add_edge(&mut self, source: NodeId, target: NodeId) -> EdgeId {
        let id = EdgeId(self.edges.len());
        self.edges.push(GraphEdge { id, source, target });
        id
    }

    pub fn toggle_node(&mut self, id: NodeId) {
        if let Some(node) = self.nodes.get_mut(id.0) {
            node.expanded = !node.expanded;
            self.recompute_visibility();
        }
    }

    /// DFS to find all nodes whose parents are all expanded
    pub fn recompute_visibility(&mut self) {
        let mut visible = Vec::new();
        let mut stack: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|n| n.parent.is_none())
            .map(|n| n.id)
            .collect();

        let mut visited = HashSet::new();

        while let Some(current_id) = stack.pop() {
            visible.push(current_id);
            visited.insert(current_id);

            let node = &self.nodes[current_id.0];
            if node.expanded {
                for &child_id in &node.children {
                    stack.push(child_id);
                }
            }
        }
        self.active_nodes = visible;
    }
}
