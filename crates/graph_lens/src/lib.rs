use gpui::{
    Action, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollWheelEvent, Window, actions, canvas, prelude::*, px,
};
use project::Project;
use ui::{IconName, Label, prelude::*};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

// Define an action to let users toggle your panel via the command palette or keybinds
actions!(graph_lens, [ToggleFocus]);

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, window: Option<&mut Window>, cx| {
            // Safely extract the window. If there's no window, bail out!
            let Some(window) = window else {
                return;
            };

            let project = workspace.project().clone();

            // 1. Create the panel entity
            let panel = cx.new(|cx| GraphLensPanel::new(project, cx));

            // 2. Add it to the workspace (this is what tells the Dock to render the icon!)
            workspace.add_panel(panel, window, cx);

            // 3. Register the action to toggle focus via the command palette / keybinds
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                if !workspace.toggle_panel_focus::<GraphLensPanel>(window, cx) {
                    workspace.close_panel::<GraphLensPanel>(window, cx);
                }
            });
        },
    )
    .detach();
}

/// Represents the camera looking at the infinite canvas
pub struct Viewport {
    pub pan: Point<f32>,
    pub zoom: f32, // e.g., 1.0 is 100% scale
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pan: Point::default(),
            zoom: 1.0,
        }
    }
}

/// A structured representation of the filesystem
pub struct GraphNode {
    pub name: String,
    pub is_dir: bool,
    pub is_expanded: bool,
    pub world_position: Point<f32>, // Absolute position in the infinite grid
    pub children: Vec<GraphNode>,
}

pub struct GraphLensPanel {
    _project: Entity<Project>,
    focus_handle: FocusHandle,
    viewport: Viewport,
    nodes: Vec<GraphNode>,
    last_mouse_position: Option<Point<f32>>,
    is_panning: bool,
}

impl GraphLensPanel {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let mut nodes = Vec::new();

        let project_handle = project.read(cx);
        let x = 100.0;
        let mut y = 100.0;

        for worktree in project_handle.worktrees(cx) {
            let worktree = worktree.read(cx);
            for entry in worktree.entries(false, 0) {
                if entry.path.is_empty() {
                    let mut node = GraphNode {
                        name: entry.path.as_unix_str().to_string(),
                        is_dir: entry.is_dir(),
                        is_expanded: false,
                        world_position: Point::new(x, y),
                        children: Vec::new(),
                    };

                    // Basic recursive population for the first level
                    if entry.is_dir() {
                        for child_entry in worktree.child_entries(&entry.path) {
                            node.children.push(GraphNode {
                                name: child_entry
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string())
                                    .unwrap_or_default(),
                                is_dir: child_entry.is_dir(),
                                is_expanded: false,
                                world_position: Point::new(x + 200.0, y), // Just a placeholder
                                children: Vec::new(),
                            });
                        }
                    }

                    nodes.push(node);
                    y += 200.0;
                }
            }
        }

        let mut this = Self {
            _project: project,
            focus_handle: cx.focus_handle(),
            viewport: Viewport::default(),
            nodes,
            last_mouse_position: None,
            is_panning: false,
        };
        this.layout();
        this
    }

    fn on_scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(1.0)).y.as_f32();
        let zoom_speed = 0.001;
        self.viewport.zoom *= 1.0 + delta * zoom_speed;
        self.viewport.zoom = self.viewport.zoom.clamp(0.1, 10.0);
        cx.notify();
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _cx: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            self.is_panning = true;
        }
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _cx: &mut Context<Self>) {
        self.is_panning = false;
        self.last_mouse_position = None;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.is_panning {
            let current_pos = event.position.map(|p| p.as_f32());
            if let Some(last_pos) = self.last_mouse_position {
                let delta = current_pos - last_pos;
                self.viewport.pan += delta;
                cx.notify();
            }
            self.last_mouse_position = Some(current_pos);
        }
    }

    fn world_to_screen(&self, world_pos: Point<f32>) -> Point<Pixels> {
        Point::new(
            px(world_pos.x * self.viewport.zoom + self.viewport.pan.x),
            px(world_pos.y * self.viewport.zoom + self.viewport.pan.y),
        )
    }

    fn toggle_expanded(&mut self, node_name: String, cx: &mut Context<Self>) {
        fn toggle_recursive(nodes: &mut Vec<GraphNode>, name: &str) -> bool {
            for node in nodes {
                if node.name == name && node.is_dir {
                    node.is_expanded = !node.is_expanded;
                    return true;
                }
                if toggle_recursive(&mut node.children, name) {
                    return true;
                }
            }
            false
        }

        if toggle_recursive(&mut self.nodes, &node_name) {
            self.layout();
            cx.notify();
        }
    }

    fn layout(&mut self) {
        let mut current_y = 100.0;
        let x = 100.0;
        let mut nodes = std::mem::take(&mut self.nodes);
        for node in &mut nodes {
            let size = Self::calculate_node_layout_static(node, Point::new(x, current_y));
            current_y += size.y + 20.0;
        }
        self.nodes = nodes;
    }

    fn calculate_node_layout_static(node: &mut GraphNode, pos: Point<f32>) -> Point<f32> {
        node.world_position = pos;
        let header_height = 40.0;
        let mut size = Point::new(150.0, header_height);

        if node.is_dir && node.is_expanded {
            let mut child_y = pos.y + header_height + 10.0;
            let child_x = pos.x + 20.0;
            let mut max_child_width: f32 = 100.0;

            for child in &mut node.children {
                let child_size =
                    Self::calculate_node_layout_static(child, Point::new(child_x, child_y));
                child_y += child_size.y + 10.0;
                max_child_width = max_child_width.max(child_size.x);
            }
            size.y = (child_y - pos.y).max(header_height + 20.0);
            size.x = (max_child_width + 40.0).max(size.x);
        } else if node.is_dir && !node.is_expanded {
            size.y += node.children.len() as f32 * 20.0;
        }

        size
    }

    fn render_node(&self, node: &GraphNode, cx: &mut Context<Self>) -> AnyElement {
        let screen_pos = self.world_to_screen(node.world_position);
        let node_name = node.name.clone();
        let zoom = self.viewport.zoom;

        // Calculate height for the container if collapsed
        let content =
            if node.is_dir {
                if node.is_expanded {
                    // When expanded, children are rendered as separate top-level nodes (handled in Render::render)
                    div()
                } else {
                    div().p_1().children(node.children.iter().map(|child| {
                        Label::new(format!("File : {}", child.name)).into_any_element()
                    }))
                }
            } else {
                div()
            };

        div()
            .absolute()
            .left(screen_pos.x)
            .top(screen_pos.y)
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_sm()
            .min_w(px(120.0 * zoom))
            .flex()
            .flex_col()
            .child(
                div()
                    .bg(cx.theme().colors().element_background)
                    .p_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .flex()
                    .justify_between()
                    .child(Label::new(format!(
                        "{} : {}",
                        if node.is_dir { "Dir." } else { "File" },
                        node.name
                    )))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, _window, cx| {
                            this.toggle_expanded(node_name.clone(), cx);
                        }),
                    ),
            )
            .child(content)
            .into_any_element()
    }
}

// 1. Required to be a Panel: Emit events and hold focus
impl EventEmitter<PanelEvent> for GraphLensPanel {}

impl Focusable for GraphLensPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// 2. How it looks (GPUI Rendering)
impl Render for GraphLensPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom = self.viewport.zoom;
        let pan = self.viewport.pan;
        let view = cx.entity();

        let mut all_nodes = Vec::new();
        fn collect_nodes<'a>(nodes: &'a [GraphNode], all: &mut Vec<&'a GraphNode>) {
            for node in nodes {
                all.push(node);
                if node.is_expanded {
                    collect_nodes(&node.children, all);
                }
            }
        }
        collect_nodes(&self.nodes, &mut all_nodes);

        let mut edges = Vec::new();
        fn collect_edges(nodes: &[GraphNode], edges: &mut Vec<(Point<f32>, Point<f32>)>) {
            for node in nodes {
                if node.is_expanded {
                    for child in &node.children {
                        edges.push((node.world_position, child.world_position));
                    }
                    collect_edges(&node.children, edges);
                }
            }
        }
        collect_edges(&self.nodes, &mut edges);

        let border_color = cx.theme().colors().border;

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(cx.theme().colors().panel_background)
            .on_scroll_wheel({
                let view = view.clone();
                move |event, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.on_scroll_wheel(event, cx);
                    });
                }
            })
            .on_mouse_down(MouseButton::Left, {
                let view = view.clone();
                move |event, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.on_mouse_down(event, cx);
                    });
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let view = view.clone();
                move |event, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.on_mouse_up(event, cx);
                    });
                }
            })
            .on_mouse_move({
                let view = view.clone();
                move |event, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.on_mouse_move(event, cx);
                    });
                }
            })
            .child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |_bounds, (), window, _cx| {
                        for (start_world, end_world) in edges {
                            let start = Point::new(
                                px(start_world.x * zoom + pan.x),
                                px(start_world.y * zoom + pan.y),
                            );
                            let end = Point::new(
                                px(end_world.x * zoom + pan.x),
                                px(end_world.y * zoom + pan.y),
                            );

                            let mut path = gpui::Path::new(start);
                            path.line_to(end);
                            window.paint_path(path, border_color);
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(all_nodes.iter().map(|node| self.render_node(node, cx)))
    }
}

// 3. How it behaves in the IDE (Zed Workspace Panel)
impl Panel for GraphLensPanel {
    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        false
    }

    fn set_active(&mut self, _active: bool, _window: &mut Window, _cx: &mut Context<Self>) {
        // You can store the active state here if you need to react to it,
        // e.g., self.active = active;
    }

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
        DockPosition::Right // You can change this to Bottom if you prefer
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
        // Handle dynamic resizing/positioning here if needed
    }

    fn activation_priority(&self) -> u32 {
        1000
    }

    // THIS is what physically puts the icon in the bottom bar
    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::FileTree)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Toggle Graph Lens")
    }

    // This links the UI button back to the action you registered in `init`
    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }
}
