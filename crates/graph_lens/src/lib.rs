use gpui::{
    Action, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rems, Render,
    ScrollWheelEvent, Window, actions, prelude::*, px,
};
use project::{Project, ProjectEntryId, WorktreeId};
use ui::{Color, IconName, Label, LabelSize, prelude::*};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

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

// ─── Layout constants ────────────────────────────────────────────────

const NODE_W: f32 = 200.0;
const HEADER_H: f32 = 30.0;
const ATTR_H: f32 = 22.0;
const ATTR_BOTTOM_PAD: f32 = 6.0;
const INNER_PAD: f32 = 14.0;
const CHILD_GAP: f32 = 10.0;
const ROOT_GAP: f32 = 24.0;
const BASE_REM: Rems = rems(12.0);

// ─── Viewport ────────────────────────────────────────────────────────

pub struct Viewport {
    pub pan: Point<f32>,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pan: Point::new(20.0, 20.0),
            zoom: 1.0,
        }
    }
}

// ─── Data model ──────────────────────────────────────────────────────

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

/// Compute world-space position and size for `node` and all descendants.
fn layout_node(node: &mut GraphNode, pos: Point<f32>) {
    node.world_position = pos;

    if !node.is_dir {
        node.world_size = Point::new(NODE_W, HEADER_H);
        return;
    }

    if !node.is_expanded {
        let rows = node.children.len() as f32;
        let h = HEADER_H
            + if rows > 0.0 {
                rows * ATTR_H + ATTR_BOTTOM_PAD
            } else {
                0.0
            };
        node.world_size = Point::new(NODE_W, h);
        return;
    }

    let child_x = pos.x + INNER_PAD;
    let mut child_y = pos.y + HEADER_H + INNER_PAD;
    let mut max_child_w: f32 = 0.0;

    for child in &mut node.children {
        layout_node(child, Point::new(child_x, child_y));
        child_y += child.world_size.y + CHILD_GAP;
        max_child_w = max_child_w.max(child.world_size.x);
    }

    let total_h = if node.children.is_empty() {
        HEADER_H + INNER_PAD * 2.0
    } else {
        (child_y - CHILD_GAP + INNER_PAD) - pos.y
    };
    let total_w = (max_child_w + INNER_PAD * 2.0).max(NODE_W);

    node.world_size = Point::new(total_w, total_h);
}

// ─── Panel ───────────────────────────────────────────────────────────

pub struct GraphLensPanel {
    project: Entity<Project>,
    focus_handle: FocusHandle,
    viewport: Viewport,
    nodes: Vec<GraphNode>,
    last_mouse_pos: Option<Point<f32>>,
    is_panning: bool,
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
                _ => {}
            },
        )
        .detach();

        let mut this = Self {
            project,
            focus_handle: cx.focus_handle(),
            viewport: Viewport::default(),
            nodes: Vec::new(),
            last_mouse_pos: None,
            is_panning: false,
        };
        this.update_nodes(cx);
        this
    }

    fn update_nodes(&mut self, cx: &mut Context<Self>) {
        let mut new_nodes = Vec::new();
        let project = self.project.read(cx);

        for worktree in project.worktrees(cx) {
            let worktree = worktree.read(cx);
            if let Some(root) = worktree.root_entry() {
                let is_expanded = self
                    .nodes
                    .iter()
                    .find(|n| n.entry_id == root.id)
                    .map(|n| n.is_expanded)
                    .unwrap_or(true);

                let mut node = GraphNode {
                    name: worktree
                        .abs_path()
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Root".to_string()),
                    worktree_id: worktree.id(),
                    entry_id: root.id,
                    is_dir: root.is_dir(),
                    is_expanded,
                    world_position: Point::default(),
                    world_size: Point::default(),
                    children: Vec::new(),
                };

                self.populate_children(&mut node, &worktree);
                new_nodes.push(node);
            }
        }

        self.nodes = new_nodes;
        self.layout();
        cx.notify();
    }

    /// Recursively fill `node.children` from the worktree.
    ///
    /// Expanded child dirs recurse fully. Collapsed child dirs populate
    /// their immediate children so the attribute list is never empty.
    fn populate_children(&self, node: &mut GraphNode, worktree: &project::Worktree) {
        if !node.is_dir {
            return;
        }
        let Some(entry) = worktree.entry_for_id(node.entry_id) else {
            return;
        };
        for child_entry in worktree.child_entries(&entry.path) {
            let is_expanded = self
                .find_old_expanded_state(child_entry.id)
                .unwrap_or(false);

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

            if child.is_dir {
                if is_expanded {
                    self.populate_children(&mut child, worktree);
                } else {
                    self.populate_immediate_children(&mut child, worktree);
                }
            }

            node.children.push(child);
        }
    }

    fn populate_immediate_children(&self, node: &mut GraphNode, worktree: &project::Worktree) {
        let Some(entry) = worktree.entry_for_id(node.entry_id) else {
            return;
        };
        for child_entry in worktree.child_entries(&entry.path) {
            let is_expanded = self
                .find_old_expanded_state(child_entry.id)
                .unwrap_or(false);
            node.children.push(GraphNode {
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
            });
        }
    }

    fn find_old_expanded_state(&self, id: ProjectEntryId) -> Option<bool> {
        fn search(nodes: &[GraphNode], id: ProjectEntryId) -> Option<bool> {
            for n in nodes {
                if n.entry_id == id {
                    return Some(n.is_expanded);
                }
                if let Some(s) = search(&n.children, id) {
                    return Some(s);
                }
            }
            None
        }
        search(&self.nodes, id)
    }

    fn layout(&mut self) {
        let mut nodes = std::mem::take(&mut self.nodes);
        let mut y = 60.0;
        for node in &mut nodes {
            layout_node(node, Point::new(60.0, y));
            y += node.world_size.y + ROOT_GAP;
        }
        self.nodes = nodes;
    }

    // ── Input handlers ─────────────────────────────────────────────

    fn on_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let dy = event.delta.pixel_delta(px(1.0)).y.as_f32();
        self.viewport.zoom *= 1.0 + dy * 0.002;
        self.viewport.zoom = self.viewport.zoom.clamp(0.1, 5.0);
        cx.notify();
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _cx: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            self.is_panning = true;
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _cx: &mut Context<Self>) {
        self.is_panning = false;
        self.last_mouse_pos = None;
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
        fn toggle(nodes: &mut Vec<GraphNode>, id: ProjectEntryId) -> bool {
            for n in nodes {
                if n.entry_id == id && n.is_dir {
                    n.is_expanded = !n.is_expanded;
                    return true;
                }
                if toggle(&mut n.children, id) {
                    return true;
                }
            }
            false
        }
        if toggle(&mut self.nodes, entry_id) {
            self.update_nodes(cx);
        }
    }

    // ── Coordinate helper ──────────────────────────────────────────

    fn w2s(&self, world: Point<f32>) -> Point<Pixels> {
        Point::new(
            px(world.x * self.viewport.zoom + self.viewport.pan.x),
            px(world.y * self.viewport.zoom + self.viewport.pan.y),
        )
    }

    // ── Render helpers ─────────────────────────────────────────────

    /// Toolbar: title + zoom controls. Lives above the canvas div so
    /// toolbar clicks never start a canvas pan.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let zoom_pct = (self.viewport.zoom * 100.0).round() as u32;
        let bg = cx.theme().colors().element_background;
        let border = cx.theme().colors().border;
        let _zoom = self.viewport.zoom;
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
                div().px_1().child(
                    Label::new("Graph Lens")
                        .size(label_size)
                        .color(Color::Muted),
                ),
            )
            .child(div().w(px(1.0)).h_4().mx_1().bg(border))
            .child(btn("−").on_mouse_down(MouseButton::Left, {
                let view = view.clone();
                move |_, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.viewport.zoom = (this.viewport.zoom - 0.1).max(0.1);
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
                move |_, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.viewport.zoom = (this.viewport.zoom + 0.1).min(5.0);
                        cx.notify();
                    });
                }
            }))
            .child(div().w(px(1.0)).h_4().mx_1().bg(border))
            .child(btn("Reset").on_mouse_down(MouseButton::Left, {
                let view = view.clone();
                move |_, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.viewport = Viewport::default();
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
                    .h(px(HEADER_H * z))
                    .px(px(8.0 * z))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(6.0 * z))
                    .bg(cx.theme().colors().element_background)
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.toggle_expanded(entry_id, cx);
                        }),
                    )
                    .child(Label::new("▼").size(LabelSize::XSmall))
                    .child(Label::new(format!("Dir. : {}", node.name)).size(LabelSize::XSmall)),
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

        let type_prefix = if is_dir { "Dir." } else { "File" };
        let expand_icon = if is_dir { "▶" } else { "  " };

        let header = div()
            .h(px(HEADER_H * z))
            .px(px(8.0 * z))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.0 * z))
            .bg(cx.theme().colors().element_background)
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .when(is_dir, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
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
            for child in &node.children {
                let child_prefix = if child.is_dir { "Dir." } else { "File" };
                card = card.child(
                    div()
                        .h(px(ATTR_H * z))
                        .px(px(14.0 * z))
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().colors().border)
                        .child(
                            Label::new(format!("{child_prefix} : {}", child.name))
                                .size(label_size)
                                .color(Color::Muted),
                        ),
                );
            }
            if !node.children.is_empty() {
                card = card.child(div().h(px(ATTR_BOTTOM_PAD * z)).flex_shrink_0());
            }
        }

        card.into_any_element()
    }
}

// ─── GPUI trait impls ────────────────────────────────────────────────

impl EventEmitter<PanelEvent> for GraphLensPanel {}

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

        fn collect<'a>(
            nodes: &'a [GraphNode],
            expanded: &mut Vec<&'a GraphNode>,
            leaves: &mut Vec<&'a GraphNode>,
        ) {
            for n in nodes {
                if n.is_dir && n.is_expanded {
                    expanded.push(n);
                    collect(&n.children, expanded, leaves);
                } else {
                    leaves.push(n);
                }
            }
        }
        collect(&self.nodes, &mut expanded, &mut leaves);

        let toolbar = self.render_toolbar(cx);

        let expanded_els: Vec<AnyElement> = expanded
            .iter()
            .map(|n| self.render_expanded_dir(n, cx))
            .collect();

        let leaf_els: Vec<AnyElement> = leaves
            .iter()
            .map(|n| self.render_leaf_node(n, cx))
            .collect();

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
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
                    .children(expanded_els)
                    .children(leaf_els),
            )
    }
}

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
