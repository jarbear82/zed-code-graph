// src/layout_fruchterman_reingold.rs

use crate::constants::*;
use crate::graph_engine::{GraphEngine, NodeId};
use crate::quadtree::QuadTree;
use gpui::{Bounds, Point, Size};
use std::collections::HashMap;

pub struct FruchtermanReingold {
    pub k: f32,    // Optimal distance between nodes
    pub temp: f32, // "Temperature" (step size), decreases over time
    pub dt: f32,   // Time step
}

impl FruchtermanReingold {
    pub fn new(node_count: usize, width: f32, height: f32) -> Self {
        let area = width * height;
        let k = (area / (node_count as f32).max(1.0)).sqrt();
        Self {
            k,
            temp: 1.0,
            dt: 0.1,
        }
    }

    pub fn update(&mut self, engine: &mut GraphEngine) {
        let active_ids = engine.active_nodes.clone();
        if active_ids.is_empty() {
            return;
        }

        let mut forces: HashMap<NodeId, Point<f32>> = HashMap::new();
        for &id in &active_ids {
            forces.insert(id, Point::default());
        }

        // ==========================================
        // 1. Repulsion (Barnes-Hut via QuadTree)
        // ==========================================
        let mut min_x = f32::MAX;
        let mut max_x = f32::MIN;
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;

        for &id in &active_ids {
            let node = &engine.nodes[id.0];
            min_x = min_x.min(node.position.x);
            max_x = max_x.max(node.position.x);
            min_y = min_y.min(node.position.y);
            max_y = max_y.max(node.position.y);
        }

        let world_size = (max_x - min_x).max(max_y - min_y).max(100.0);
        let mut qt = QuadTree::new(Bounds {
            origin: Point {
                x: min_x - OVERLAP_DISTANCE_THRESHOLD,
                y: min_y - OVERLAP_DISTANCE_THRESHOLD,
            },
            size: Size {
                width: world_size + 100.0,
                height: world_size + 100.0,
            },
        });

        // Insert all active nodes into the QuadTree
        for &id in &active_ids {
            let node = &engine.nodes[id.0];
            let mass = (node.size.width * node.size.height).sqrt() / MAX_VELOCITY;
            // Note: QuadTree::insert will need to accept NodeId instead of hecs::Entity
            qt.insert(id, node.position, mass);
        }

        // Calculate repulsive force for each node against the QuadTree
        for &id in &active_ids {
            let pos = engine.nodes[id.0].position;
            qt.calculate_repulsion(
                id,
                pos,
                POSITION_CHANGE_THRESHOLD,
                REPULSION_STRENGTH,
                &mut forces,
            );
        }

        // ==========================================
        // 1.5 Overlap Correction
        // ==========================================
        for i in 0..active_ids.len() {
            for j in i + 1..active_ids.len() {
                let id1 = active_ids[i];
                let id2 = active_ids[j];
                let n1 = &engine.nodes[id1.0];
                let n2 = &engine.nodes[id2.0];

                // Don't separate parent and child nodes explicitly here
                if n1.parent == Some(id2) || n2.parent == Some(id1) {
                    continue;
                }

                let c1 = Point {
                    x: n1.position.x + n1.size.width / 2.0,
                    y: n1.position.y + n1.size.height / 2.0,
                };
                let c2 = Point {
                    x: n2.position.x + n2.size.width / 2.0,
                    y: n2.position.y + n2.size.height / 2.0,
                };

                let mut dx = c1.x - c2.x;
                let mut dy = c1.y - c2.y;

                if dx.abs() < OVERLAP_DISTANCE_THRESHOLD && dy.abs() < OVERLAP_DISTANCE_THRESHOLD {
                    let seed = (id1.0 as f32 * 0.618 + id2.0 as f32 * 0.382).sin();
                    let angle = seed * std::f32::consts::PI * 2.0;
                    dx = angle.cos() * PUSH_DISTANCE;
                    dy = angle.sin() * PUSH_DISTANCE;
                }

                let overlap_x =
                    (n1.size.width + n2.size.width) / 2.0 + COMPOUND_PADDING_OTHER - dx.abs();
                let overlap_y =
                    (n1.size.height + n2.size.height) / 2.0 + COMPOUND_PADDING_OTHER - dy.abs();

                if overlap_x > 0.0 && overlap_y > 0.0 {
                    let dist_sq = (dx * dx + dy * dy).max(MIN_OVERLAP_DISTANCE_SQ);
                    let dist = dist_sq.sqrt();
                    let force = (overlap_x * overlap_y).sqrt() * OVERLAP_CORRECTION_STIFFNESS;
                    let fx = (dx / dist) * force;
                    let fy = (dy / dist) * force;

                    forces.get_mut(&id1).unwrap().x += fx;
                    forces.get_mut(&id1).unwrap().y += fy;
                    forces.get_mut(&id2).unwrap().x -= fx;
                    forces.get_mut(&id2).unwrap().y -= fy;
                }
            }
        }

        // ==========================================
        // 2. Attractive Forces (Springs)
        // ==========================================
        for edge in &engine.edges {
            if active_ids.contains(&edge.source) && active_ids.contains(&edge.target) {
                let n1 = &engine.nodes[edge.source.0];
                let n2 = &engine.nodes[edge.target.0];

                let cx1 = n1.position.x + n1.size.width / 2.0;
                let cy1 = n1.position.y + n1.size.height / 2.0;
                let cx2 = n2.position.x + n2.size.width / 2.0;
                let cy2 = n2.position.y + n2.size.height / 2.0;

                let dx = cx2 - cx1;
                let dy = cy2 - cy1;
                let dist_sq = (dx * dx + dy * dy).max(MIN_OVERLAP_DISTANCE_SQ);
                let dist = dist_sq.sqrt();

                let force = (dist - IDEAL_EDGE_LENGTH) * SPRING_STRENGTH;
                let fx = (dx / dist) * force;
                let fy = (dy / dist) * force;

                forces.get_mut(&edge.source).unwrap().x += fx;
                forces.get_mut(&edge.source).unwrap().y += fy;
                forces.get_mut(&edge.target).unwrap().x -= fx;
                forces.get_mut(&edge.target).unwrap().y -= fy;
            }
        }

        // ==========================================
        // 3. Integrate
        // ==========================================
        let center = Point {
            x: engine.bounds.x / 2.0,
            y: engine.bounds.y / 2.0,
        };

        for &id in &active_ids {
            let force = forces[&id];
            let node = &mut engine.nodes[id.0];

            // Skip integration for nodes the user is currently interacting with
            if node.is_fixed {
                continue;
            }

            let gx = (center.x - node.position.x) * GRAVITY_STRENGTH;
            let gy = (center.y - node.position.y) * GRAVITY_STRENGTH;

            let velocity_damping = 0.8;
            node.velocity.x = (node.velocity.x + force.x + gx) * velocity_damping;
            node.velocity.y = (node.velocity.y + force.y + gy) * velocity_damping;

            node.velocity.x = node.velocity.x.clamp(-VELOCITY_CLAMP, VELOCITY_CLAMP);
            node.velocity.y = node.velocity.y.clamp(-VELOCITY_CLAMP, VELOCITY_CLAMP);

            node.position.x += node.velocity.x * self.temp;
            node.position.y += node.velocity.y * self.temp;
        }

        // ==========================================
        // 3.5 Dynamic Parent Resizing
        // ==========================================
        let mut new_parent_sizes: HashMap<NodeId, Size<f32>> = HashMap::new();
        let parent_ids: Vec<NodeId> = active_ids
            .iter()
            .filter(|&&id| {
                let kind = &engine.nodes[id.0].kind;
                *kind == crate::graph_engine::NodeKind::Directory
                    || *kind == crate::graph_engine::NodeKind::Worktree
            })
            .copied()
            .collect();

        for pid in parent_ids {
            let mut min_x = f32::MAX;
            let mut max_x = f32::MIN;
            let mut min_y = f32::MAX;
            let mut max_y = f32::MIN;
            let mut has_children = false;

            for &child_id in &active_ids {
                let node = &engine.nodes[child_id.0];
                if node.parent == Some(pid) {
                    min_x = min_x.min(node.position.x);
                    max_x = max_x.max(node.position.x + node.size.width);
                    min_y = min_y.min(node.position.y);
                    max_y = max_y.max(node.position.y + node.size.height);
                    has_children = true;
                }
            }

            if has_children {
                let target_w = (max_x - min_x) + COMPOUND_PADDING_OTHER * 2.0;
                let target_h = (max_y - min_y) + COMPOUND_PADDING_TOP + COMPOUND_PADDING_OTHER;

                let target_w = target_w.max(PARENT_MIN_WIDTH);
                let target_h = target_h.max(PARENT_MIN_HEIGHT);

                let parent = &engine.nodes[pid.0];
                let new_w =
                    parent.size.width + (target_w - parent.size.width) * PARENT_SIZE_LERP_FACTOR;
                let new_h =
                    parent.size.height + (target_h - parent.size.height) * PARENT_SIZE_LERP_FACTOR;

                new_parent_sizes.insert(
                    pid,
                    Size {
                        width: new_w,
                        height: new_h,
                    },
                );
            }
        }

        for (pid, new_size) in new_parent_sizes {
            engine.nodes[pid.0].size = new_size;
        }

        // ==========================================
        // 4. Hierarchical Constraints
        // ==========================================
        let mut parent_info = HashMap::new();
        for &id in &active_ids {
            let node = &engine.nodes[id.0];
            if node.parent.is_none()
                || node.kind == crate::graph_engine::NodeKind::Directory
                || node.kind == crate::graph_engine::NodeKind::Worktree
            {
                parent_info.insert(id, (node.position, node.size));
            }
        }

        for &id in &active_ids {
            let pid = engine.nodes[id.0].parent;
            if let Some(pid) = pid {
                if let Some(&(p_pos, p_size)) = parent_info.get(&pid) {
                    let node = &mut engine.nodes[id.0];

                    let min_x = p_pos.x + COMPOUND_PADDING_OTHER;
                    let max_x = p_pos.x + p_size.width - node.size.width - COMPOUND_PADDING_OTHER;
                    let min_y = p_pos.y + COMPOUND_PADDING_TOP;
                    let max_y = p_pos.y + p_size.height - node.size.height - COMPOUND_PADDING_OTHER;

                    // Guarantee bounds are coherent even if child is currently larger than parent
                    let max_x = max_x.max(min_x);
                    let max_y = max_y.max(min_y);

                    node.position.x = node.position.x.clamp(min_x, max_x);
                    node.position.y = node.position.y.clamp(min_y, max_y);
                }
            }
        }

        // Cool down the simulation
        self.temp *= COOLING_FACTOR;
    }
}
