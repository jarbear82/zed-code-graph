use fcose::{
    graph::{CompoundGraph, Node as FcoseNode, NodeId},
    run_layout,
};
use gpui::{
    Action, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollWheelEvent, Window, actions, prelude::*, px,
};
use project::{Project, ProjectEntryId, WorktreeId};
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Add;
use ui::{Color, IconName, Label, LabelSize, prelude::*};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

mod fcose;

actions!(graph_lens, [ToggleFocus]);

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, window: Option<&mut Window>, cx| {
            let Some(window) = window else { return };
            let project = workspace.project().clone();
            let panel = cx.new(|cx| GraphLensPanel::new(project, window, cx));
            workspace.add_panel(panel, window, cx);
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                if !workspace.toggle_panel_focus::<GraphLensPanel>(window, cx) {
                    workspace.close_panel::<GraphLensPanel>(window, cx);
                }
            });
        },
    )
    .detach();
}

// ─── Constants ───

const NODE_W: f32 = 200.0;
const HEADER_H: f32 = 30.0;
const ATTR_H: f32 = 22.0;
const ATTR_BOTTOM_PAD: f32 = 6.0;

const DEFAULT_PAN: Point<f32> = Point::new(20.0, 20.0);
const DEFAULT_ZOOM: f32 = 1.0;
const ZOOM_MIN: f32 = 0.1;
const ZOOM_MAX: f32 = 5.0;
const ZOOM_CLICK_STEP: f32 = 0.1;
const ZOOM_SCROLL_SENSITIVITY: f32 = 0.002;

const HEADER_PAD: f32 = 8.0;
const HEADER_GAP: f32 = 6.0;
const ATTR_PAD: f32 = 14.0;

// ─── Layout config ───

struct LayoutConfig {
    node_width: f32,
    header_height: f32,
    attr_height: f32,
    attr_bottom_pad: f32,
    header_pad: f32,
    header_gap: f32,
    attr_pad: f32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            node_width: NODE_W,
            header_height: HEADER_H,
            attr_height: ATTR_H,
            attr_bottom_pad: ATTR_BOTTOM_PAD,
            header_pad: HEADER_PAD,
            header_gap: HEADER_GAP,
            attr_pad: ATTR_PAD,
        }
    }
}

// ─── Viewport ───

pub struct Viewport {
    pub pan: Point<f32>,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pan: DEFAULT_PAN,
            zoom: DEFAULT_ZOOM,
        }
    }
}

// ─── Data model ───

pub struct GraphNode {
    pub name: String,
    pub worktree_id: WorktreeId,
    pub entry_id: ProjectEntryId,
    pub is_dir: bool,
    pub is_expanded: bool,
    /// Top-left corner in world space (set by `layout_node`).
    pub world_position: Point<f32>,
    /// Bounding box in world space (set by `layout_node`).
    pub world_size: Point<f32>,
    pub children: Vec<GraphNode>,
}

// ─── Panel ───

pub struct GraphLensPanel {
    project: Entity<Project>,
    focus_handle: FocusHandle,
    viewport: Viewport,
    nodes: Vec<GraphNode>,
    dependencies: Vec<(ProjectEntryId, ProjectEntryId)>,
    expanded_set: HashSet<ProjectEntryId>,
    last_mouse_pos: Option<Point<f32>>,
    is_panning: bool,
    config: LayoutConfig,
    root_name: Option<String>,
}

impl GraphLensPanel {
    pub fn new(project: Entity<Project>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.subscribe_in(
            &project,
            window,
            |this, _, event, _window, cx| match event {
                project::Event::WorktreeUpdatedEntries(_, _)
                | project::Event::WorktreeAdded(_)
                | project::Event::WorktreeRemoved(_)
                | project::Event::WorktreeOrderChanged => {
                    this.update_nodes(cx);
                }
                _ => {
                    // Intentionally ignored: other project events don't affect
                    // the graph lens tree structure.
                }
            },
        )
        .detach();

        let mut this = Self {
            project,
            focus_handle: cx.focus_handle(),
            viewport: Viewport::default(),
            nodes: Vec::new(),
            dependencies: Vec::new(),
            expanded_set: HashSet::new(),
            last_mouse_pos: None,
            is_panning: false,
            config: LayoutConfig::default(),
            root_name: None,
        };
        this.update_nodes(cx);
        this
    }

    fn update_nodes(&mut self, cx: &mut Context<Self>) {
        let mut new_nodes = Vec::new();
        let project = self.project.read(cx);

        // Determine if we should hide the root and show its children instead.
        let visible_worktrees: Vec<_> = project.visible_worktrees(cx).collect();
        let hide_root = visible_worktrees.len() == 1;

        // Extract the root name for toolbar display when hiding the root.
        self.root_name = if hide_root {
            visible_worktrees.first().and_then(|wt| {
                let path = wt.read(cx).abs_path();
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
        } else {
            None
        };

        // =========================================================
        // 1. Build the File Tree (Compound Nodes / Bounding Boxes)
        // =========================================================
        for worktree in project.worktrees(cx) {
            let worktree = worktree.read(cx);
            if let Some(root) = worktree.root_entry() {
                let entries_to_process: Vec<_> = if hide_root {
                    worktree.child_entries(&root.path).collect()
                } else {
                    vec![root]
                };

                for child_entry in entries_to_process {
                    let is_expanded = self.expanded_set.contains(&child_entry.id);

                    let mut node = GraphNode {
                        name: child_entry
                            .path
                            .file_name()
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                        worktree_id: worktree.id(),
                        entry_id: child_entry.id,
                        is_dir: child_entry.is_dir(),
                        is_expanded,
                        world_position: Point::default(),
                        world_size: Point::default(),
                        children: Vec::new(),
                    };

                    if node.is_dir && is_expanded {
                        self.populate_tree(&mut node, &worktree);
                    }

                    new_nodes.push(node);
                }
            }
        }

        // =========================================================
        // 2. Gather Dependencies (fCoSE Adjacency Edges / Springs)
        // =========================================================
        let mut new_deps = Vec::new();

        // TODO: Hook up your Language Server or Tree-sitter parser here.
        // Once you extract which files import which other files, push them
        // as a tuple of (SourceEntryId, TargetEntryId) into `new_deps`.
        //
        // Example conceptual implementation:
        // for worktree in project.worktrees(cx) {
        //     let wt = worktree.read(cx);
        //     for file in wt.files(cx) {
        //         // Ask the project/LSP for imports found in this file
        //         if let Some(imported_files) = project.get_imports(file.id, cx) {
        //             for target_file_id in imported_files {
        //                 new_deps.push((file.id, target_file_id));
        //             }
        //         }
        //     }
        // }

        // Quick dummy test: Connect the 1st file to the 2nd file in your tree to see them snap together
        if new_nodes.len() >= 2 {
            let file_a = new_nodes[0].entry_id;
            let file_b = new_nodes[1].entry_id;
            new_deps.push((file_a, file_b));
        }

        // =========================================================
        // 3. Update State & Run Physics Engine
        // =========================================================
        self.dependencies = new_deps;
        self.nodes = new_nodes;

        // Triggers the fCoSE algorithm in `self.layout()` using the newly
        // generated nodes and dependencies
        self.layout();

        cx.notify();
    }

    /// Recursively fill `node.children` from the worktree.
    ///
    /// Expanded child dirs recurse fully. Collapsed child dirs populate
    /// their immediate children so the attribute list is never empty.
    fn populate_tree(&self, node: &mut GraphNode, worktree: &project::Worktree) {
        if !node.is_dir {
            return;
        }
        let Some(entry) = worktree.entry_for_id(node.entry_id) else {
            return;
        };
        for child_entry in worktree.child_entries(&entry.path) {
            let is_expanded = self.expanded_set.contains(&child_entry.id);

            let mut child = GraphNode {
                name: child_entry
                    .path
                    .file_name()
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                worktree_id: worktree.id(),
                entry_id: child_entry.id,
                is_dir: child_entry.is_dir(),
                is_expanded,
                world_position: Point::default(),
                world_size: Point::default(),
                children: Vec::new(),
            };

            if child.is_dir && is_expanded {
                self.populate_tree(&mut child, worktree);
            }

            node.children.push(child);
        }
    }

    fn layout(&mut self) {
        if self.nodes.is_empty() {
            return;
        }

        let mut cg = CompoundGraph::new();
        let mut entry_to_node = HashMap::new();
        let mut node_counter = 0u32;

        // Step 1: Flatten the tree and build fCoSE nodes
        self.build_fcose_nodes(
            &self.nodes,
            None,
            &mut cg,
            &mut entry_to_node,
            &mut node_counter,
        );

        // Step 2: Add Adjacency Edges!
        // Loop through the dependencies we saved in our panel state
        for (source_entry_id, target_entry_id) in &self.dependencies {
            // Only add the edge if BOTH files are currently visible in the graph
            if let (Some(&fcose_source), Some(&fcose_target)) = (
                entry_to_node.get(source_entry_id),
                entry_to_node.get(target_entry_id),
            ) {
                cg.add_edge(fcose_source, fcose_target);
            }
        }

        // Step 3: Run the fCoSE layout engine
        run_layout(&mut cg, &[], &[], &[], 100);

        // Step 4: Map the computed positions back to GPUI world coordinates
        Self::apply_fcose_positions(&mut self.nodes, &cg, &entry_to_node);
        self.center_view();
    }

    fn center_view(&mut self) {
        if self.nodes.is_empty() {
            return;
        }

        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;

        let mut stack = vec![self.nodes.as_slice()];
        while let Some(nodes) = stack.pop() {
            for n in nodes {
                min_x = min_x.min(n.world_position.x);
                min_y = min_y.min(n.world_position.y);
                if n.is_dir && n.is_expanded {
                    stack.push(&n.children);
                }
            }
        }

        if min_x != f32::MAX && min_y != f32::MAX {
            let pad = 40.0;
            self.viewport.pan = Point::new(
                pad - min_x * self.viewport.zoom,
                pad - min_y * self.viewport.zoom,
            );
        }
    }

    /// Recursively create fCoSE nodes from the UI state
    fn build_fcose_nodes(
        &self,
        ui_nodes: &[GraphNode],
        parent_id: Option<NodeId>,
        cg: &mut CompoundGraph,
        map: &mut HashMap<ProjectEntryId, NodeId>,
        counter: &mut u32,
    ) -> Vec<NodeId> {
        let mut sibling_ids = Vec::new();

        for ui_node in ui_nodes {
            let id = NodeId(*counter);
            *counter += 1;
            map.insert(ui_node.entry_id, id);
            sibling_ids.push(id);

            // Determine dimensions based on whether it's expanded or a leaf
            let (w, h) = if ui_node.is_dir && ui_node.is_expanded {
                // fCoSE will recalculate compound bounds automatically during the physics tick,
                // so we can initialize this at 0.
                (0.0, 0.0)
            } else {
                // Leaf nodes or collapsed dirs use static sizes
                let rows = if ui_node.is_dir {
                    ui_node.children.len() as f32
                } else {
                    0.0
                };
                let h = self.config.header_height + (rows * self.config.attr_height);
                (self.config.node_width, h)
            };

            let mut fcose_node = if ui_node.is_dir && ui_node.is_expanded {
                FcoseNode::new_compound(id)
            } else {
                FcoseNode::new(id, w.into(), h.into())
            };

            fcose_node.parent_id = parent_id;

            // Recurse for children if expanded
            if ui_node.is_dir && ui_node.is_expanded {
                fcose_node.children =
                    self.build_fcose_nodes(&ui_node.children, Some(id), cg, map, counter);
            }

            cg.add_node(fcose_node);
        }

        sibling_ids
    }

    /// Recursively read the fCoSE positions back into the UI GraphNodes
    fn apply_fcose_positions(
        ui_nodes: &mut [GraphNode],
        cg: &CompoundGraph,
        map: &HashMap<ProjectEntryId, NodeId>,
    ) {
        for ui_node in ui_nodes {
            if let Some(&fcose_id) = map.get(&ui_node.entry_id) {
                let cg_node = cg.node(fcose_id);

                // fCoSE positions nodes by their CENTER.
                // GPUI expects the TOP-LEFT corner for layout.
                let top_left_x = cg_node.pos.x - (cg_node.width * 0.5);
                let top_left_y = cg_node.pos.y - (cg_node.height * 0.5);

                ui_node.world_position = Point::new(top_left_x as f32, top_left_y as f32);
                ui_node.world_size = Point::new(cg_node.width as f32, cg_node.height as f32);
            }

            if ui_node.is_dir && ui_node.is_expanded {
                Self::apply_fcose_positions(&mut ui_node.children, cg, map);
            }
        }
    }

    // ── Input handlers ───

    fn on_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let dy = event.delta.pixel_delta(px(1.0)).y.as_f32();
        let multiplier = 1.0 + dy * ZOOM_SCROLL_SENSITIVITY;
        self.viewport.zoom *= multiplier.max(0.0);
        self.viewport.zoom = self.viewport.zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        cx.notify();
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            self.is_panning = true;
            cx.stop_propagation();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, cx: &mut Context<Self>) {
        self.is_panning = false;
        self.last_mouse_pos = None;
        cx.stop_propagation();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.is_panning {
            let pos = event.position.map(|p| p.as_f32());
            if let Some(last) = self.last_mouse_pos {
                self.viewport.pan += pos - last;
                cx.notify();
            }
            self.last_mouse_pos = Some(pos);
        }
    }

    fn toggle_expanded(&mut self, entry_id: ProjectEntryId, cx: &mut Context<Self>) {
        if self.expanded_set.contains(&entry_id) {
            self.expanded_set.remove(&entry_id);
        } else {
            self.expanded_set.insert(entry_id);
        }
        self.update_nodes(cx);
    }

    // ── Coordinate helper ──

    fn w2s(&self, world: Point<f32>) -> Point<Pixels> {
        Point::new(
            px(world.x * self.viewport.zoom + self.viewport.pan.x),
            px(world.y * self.viewport.zoom + self.viewport.pan.y),
        )
    }

    /// Render the children list of a collapsed directory node.
    fn render_children_list<'a>(
        &self,
        children: &'a [GraphNode],
        cx: &Context<Self>,
    ) -> impl IntoElement + 'a {
        let z = self.viewport.zoom;
        let mut card = div().flex().flex_col();
        for child in children {
            let child_prefix = type_prefix(child.is_dir);
            card = card.child(
                div()
                    .h(px(self.config.attr_height * z))
                    .px(px(self.config.attr_pad * z))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Label::new(format!("{child_prefix} : {}", child.name))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            );
        }
        if !children.is_empty() {
            card = card.child(div().h(px(self.config.attr_bottom_pad * z)).flex_shrink_0());
        }
        card
    }

    // ── Render helpers ───

    /// Toolbar: title + zoom controls. Lives above the canvas div so
    /// toolbar clicks never start a canvas pan.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let zoom_pct = (self.viewport.zoom * 100.0).round() as u32;
        let bg = cx.theme().colors().element_background;
        let border = cx.theme().colors().border;
        let label_size = LabelSize::XSmall;

        let btn = |label: &'static str| {
            div()
                .px_2()
                .h_6()
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .bg(bg)
                .border_1()
                .border_color(border)
                .child(Label::new(label).size(label_size).color(Color::Muted))
        };

        let title = if let Some(ref name) = self.root_name {
            format!("Graph Lens — {}", name)
        } else {
            "Graph Lens".to_string()
        };

        div()
            .h_8()
            .flex()
            .flex_shrink_0()
            .items_center()
            .px_2()
            .gap_1()
            .bg(cx.theme().colors().surface_background)
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .px_1()
                    .child(Label::new(title).size(label_size).color(Color::Muted)),
            )
            .child(div().w(px(1.0)).h_4().mx_1().bg(border))
            .child(btn("−").on_mouse_down(MouseButton::Left, {
                let view = view.clone();
                move |_event, _window, cx| {
                    cx.stop_propagation();
                    view.update(cx, |this, cx| {
                        this.viewport.zoom = (this.viewport.zoom - ZOOM_CLICK_STEP).max(ZOOM_MIN);
                        cx.notify();
                    });
                }
            }))
            .child(
                div().min_w(px(44.0)).flex().justify_center().child(
                    Label::new(format!("{zoom_pct}%"))
                        .size(label_size)
                        .color(Color::Muted),
                ),
            )
            .child(btn("+").on_mouse_down(MouseButton::Left, {
                let view = view.clone();
                move |_event, _window, cx| {
                    cx.stop_propagation();
                    view.update(cx, |this, cx| {
                        this.viewport.zoom = (this.viewport.zoom + ZOOM_CLICK_STEP).min(ZOOM_MAX);
                        cx.notify();
                    });
                }
            }))
            .child(div().w(px(1.0)).h_4().mx_1().bg(border))
            .child(btn("Reset").on_mouse_down(MouseButton::Left, {
                let view = view.clone();
                move |_event, _window, cx| {
                    cx.stop_propagation();
                    view.update(cx, |this, cx| {
                        this.viewport = Viewport::default();
                        cx.notify();
                    });
                }
            }))
            .child(div().w(px(1.0)).h_4().mx_1().bg(border))
            // Add the Run Layout button
            .child(btn("Run Layout").on_mouse_down(MouseButton::Left, {
                let view = view.clone();
                move |_event, _window, cx| {
                    cx.stop_propagation();
                    view.update(cx, |this, cx| {
                        // Trigger the fCoSE algorithm and update the GPUI coordinates
                        this.layout();
                        // Tell GPUI to re-render the screen
                        cx.notify();
                    });
                }
            }))
    }

    /// Render an expanded directory as a large compound-node container.
    ///
    /// Only the header and the outer border are drawn here. Child nodes are
    /// rendered separately and float on top due to GPUI's painter's algorithm.
    fn render_expanded_dir(&self, node: &GraphNode, cx: &mut Context<Self>) -> AnyElement {
        let sp = self.w2s(node.world_position);
        let z = self.viewport.zoom;
        let w = px(node.world_size.x * z);
        let h = px(node.world_size.y * z);
        let entry_id = node.entry_id;

        let type_prefix = type_prefix(node.is_dir);
        let expand_icon = if node.is_expanded { "▼" } else { "▶" };

        div()
            .absolute()
            .left(sp.x)
            .top(sp.y)
            .w(w)
            .h(h)
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().panel_background)
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(self.config.header_height * z))
                    .px(px(self.config.header_pad * z))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(self.config.header_gap * z))
                    .bg(cx.theme().colors().element_background)
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_expanded(entry_id, cx);
                        }),
                    )
                    .child(Label::new(expand_icon).size(LabelSize::XSmall))
                    .child(
                        Label::new(format!("{type_prefix} : {}", node.name))
                            .size(LabelSize::XSmall),
                    ),
            )
            .into_any_element()
    }

    /// Render a file node or a collapsed directory as a UML class card.
    fn render_leaf_node(&self, node: &GraphNode, cx: &mut Context<Self>) -> AnyElement {
        let sp = self.w2s(node.world_position);
        let z = self.viewport.zoom;
        let w = px(node.world_size.x * z);
        let entry_id = node.entry_id;
        let is_dir = node.is_dir;
        let label_size = LabelSize::XSmall;

        let type_prefix = type_prefix(is_dir);
        let expand_icon = if is_dir { "▶" } else { "  " };

        let header = div()
            .h(px(self.config.header_height * z))
            .px(px(self.config.header_pad * z))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(self.config.header_gap * z))
            .bg(cx.theme().colors().element_background)
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .when(is_dir, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_expanded(entry_id, cx);
                    }),
                )
            })
            .child(Label::new(expand_icon).size(label_size).color(Color::Muted))
            .child(Label::new(format!("{type_prefix} : {}", node.name)).size(label_size));

        let mut card = div()
            .absolute()
            .left(sp.x)
            .top(sp.y)
            .w(w)
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_sm()
            .flex()
            .flex_col()
            .child(header);

        if is_dir && !node.is_expanded {
            card = card.child(self.render_children_list(&node.children, cx));
        }
        card.into_any_element()
    }

    fn render_edges(&self, _cx: &Context<Self>) -> AnyElement {
        let mut edge_coords = Vec::new();

        for (src_id, tgt_id) in &self.dependencies {
            let src_node = self.nodes.iter().find(|n| n.entry_id == *src_id);
            let tgt_node = self.nodes.iter().find(|n| n.entry_id == *tgt_id);

            if let (Some(src), Some(tgt)) = (src_node, tgt_node) {
                let start = self.w2s(
                    src.world_position
                        .add(Point::new(src.world_size.x / 2.0, src.world_size.y / 2.0)),
                );
                let end = self.w2s(
                    tgt.world_position
                        .add(Point::new(tgt.world_size.x / 2.0, tgt.world_size.y / 2.0)),
                );
                edge_coords.push((start, end));
            }
        }

        gpui::canvas(
            |_bounds, _window, _app| {},
            move |_bounds, _state, cx, _app| {
                for &(start, end) in &edge_coords {
                    let mut path = gpui::Path::new(start);
                    path.line_to(end);
                    cx.paint_path(path, gpui::white());
                }
            },
        )
        .size_full()
        .absolute()
        .top_0()
        .left_0()
        .into_any_element()
    }

    /// Collect expanded dirs and leaf nodes into separate lists for z-order rendering.
    fn collect_nodes<'a>(
        nodes: &'a [GraphNode],
        expanded: &mut Vec<&'a GraphNode>,
        leaves: &mut Vec<&'a GraphNode>,
    ) {
        let mut stack = vec![nodes];
        while let Some(current) = stack.pop() {
            for n in current {
                if n.is_dir && n.is_expanded {
                    expanded.push(n);
                    stack.push(&n.children);
                } else {
                    leaves.push(n);
                }
            }
        }
    }
}

fn type_prefix(is_dir: bool) -> &'static str {
    if is_dir { "Dir." } else { "File" }
}

// ─── GPUI trait impls ───

impl Focusable for GraphLensPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GraphLensPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();

        // Expanded dirs rendered first (z-bottom). Leaf nodes rendered after (z-top).
        let mut expanded: Vec<&GraphNode> = Vec::new();
        let mut leaves: Vec<&GraphNode> = Vec::new();

        Self::collect_nodes(&self.nodes, &mut expanded, &mut leaves);

        let toolbar = self.render_toolbar(cx);

        let expanded_els: Vec<AnyElement> = expanded
            .iter()
            .map(|n| self.render_expanded_dir(n, cx))
            .collect();

        let leaf_els: Vec<AnyElement> = leaves
            .iter()
            .map(|n| self.render_leaf_node(n, cx))
            .collect();

        let content = if self.nodes.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().colors().panel_background)
                .child(Label::new("No visible worktree entries.").color(Color::Muted))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .relative()
                .overflow_hidden()
                .bg(cx.theme().colors().panel_background)
                .on_scroll_wheel({
                    let view = view.clone();
                    move |event, _window, cx| {
                        view.update(cx, |this, cx| this.on_scroll_wheel(event, cx));
                    }
                })
                .on_mouse_down(MouseButton::Left, {
                    let view = view.clone();
                    move |event, _window, cx| {
                        view.update(cx, |this, cx| this.on_mouse_down(event, cx));
                    }
                })
                .on_mouse_up(MouseButton::Left, {
                    let view = view.clone();
                    move |event, _window, cx| {
                        view.update(cx, |this, cx| this.on_mouse_up(event, cx));
                    }
                })
                .on_mouse_move({
                    let view = view.clone();
                    move |event, _window, cx| {
                        view.update(cx, |this, cx| this.on_mouse_move(event, cx));
                    }
                })
                .child(self.render_edges(cx))
                .children(expanded_els)
                .children(leaf_els)
                .into_any_element()
        };

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(content)
    }
}

impl EventEmitter<PanelEvent> for GraphLensPanel {}

impl Panel for GraphLensPanel {
    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        false
    }

    fn set_active(&mut self, _active: bool, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn persistent_name() -> &'static str {
        "GraphLensPanel"
    }

    fn panel_key() -> &'static str {
        "graph_lens_panel"
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(400.)
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        DockPosition::Right
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Right | DockPosition::Bottom)
    }

    fn set_position(
        &mut self,
        _position: DockPosition,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn activation_priority(&self) -> u32 {
        1000
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::FileTree)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Toggle Graph Lens")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }
}
