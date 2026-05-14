use crate::graph_engine::NodeId;
use gpui::{Bounds, Point};
use std::collections::HashMap;

/// A spatial QuadTree used to approximate distant repulsive forces
/// (Barnes-Hut algorithm) to achieve O(N log N) layout performance.
pub struct QuadTree {
    pub bounds: Bounds<f32>,
    pub mass: f32,
    pub center_of_mass: Point<f32>,
    pub children: Option<Box<[QuadTree; 4]>>,
    pub entity: Option<NodeId>,
}

impl QuadTree {
    pub fn new(bounds: Bounds<f32>) -> Self {
        Self {
            bounds,
            mass: 0.0,
            center_of_mass: Point::default(),
            children: None,
            entity: None,
        }
    }

    pub fn insert(&mut self, entity: NodeId, pos: Point<f32>, mass: f32) {
        if self.mass == 0.0 {
            self.mass = mass;
            self.center_of_mass = pos;
            self.entity = Some(entity);
            return;
        }

        if self.children.is_none() {
            let half_size = self.bounds.size / 2.0;
            let origin = self.bounds.origin;
            let mut children = [
                QuadTree::new(Bounds {
                    origin,
                    size: half_size,
                }),
                QuadTree::new(Bounds {
                    origin: Point {
                        x: origin.x + half_size.width,
                        y: origin.y,
                    },
                    size: half_size,
                }),
                QuadTree::new(Bounds {
                    origin: Point {
                        x: origin.x,
                        y: origin.y + half_size.height,
                    },
                    size: half_size,
                }),
                QuadTree::new(Bounds {
                    origin: Point {
                        x: origin.x + half_size.width,
                        y: origin.y + half_size.height,
                    },
                    size: half_size,
                }),
            ];

            if let Some(old_entity) = self.entity.take() {
                let old_pos = self.center_of_mass;
                let old_mass = self.mass;
                for child in &mut children {
                    if child.bounds.contains(&old_pos) {
                        child.insert(old_entity, old_pos, old_mass);
                        break;
                    }
                }
            }
            self.children = Some(Box::new(children));
        }

        let total_mass = self.mass + mass;
        self.center_of_mass = Point {
            x: (self.center_of_mass.x * self.mass + pos.x * mass) / total_mass,
            y: (self.center_of_mass.y * self.mass + pos.y * mass) / total_mass,
        };
        self.mass = total_mass;

        if let Some(children) = &mut self.children {
            for child in children.iter_mut() {
                if child.bounds.contains(&pos) {
                    child.insert(entity, pos, mass);
                    break;
                }
            }
        }
    }

    pub fn calculate_repulsion(
        &self,
        entity: NodeId,
        pos: Point<f32>,
        theta: f32,
        repulsion_strength: f32,
        forces: &mut HashMap<NodeId, Point<f32>>,
    ) {
        if self.mass == 0.0 || (self.entity == Some(entity)) {
            return;
        }

        let dx = pos.x - self.center_of_mass.x;
        let dy = pos.y - self.center_of_mass.y;
        let dist_sq = (dx * dx + dy * dy).max(1.0); // Clamp minimum distance squared
        let dist = dist_sq.sqrt();

        let width = self.bounds.size.width;

        // Barnes-Hut condition: if the node is far enough away, treat the entire
        // quadrant as a single super-node.
        if self.children.is_none() || (width / dist < theta) {
            let force = (repulsion_strength * self.mass) / dist_sq;
            let fx = (dx / dist) * force;
            let fy = (dy / dist) * force;

            if let Some(f) = forces.get_mut(&entity) {
                f.x += fx;
                f.y += fy;
            }
        } else if let Some(children) = &self.children {
            for child in children.iter() {
                child.calculate_repulsion(entity, pos, theta, repulsion_strength, forces);
            }
        }
    }
}
