// src/lib.rs

use editor::Editor;
use gpui::{
    AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyContext, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, Pixels, Point, Render, ScrollWheelEvent, Size, Styled, Subscription, Task,
    UniformListScrollHandle, Window, actions, div, px, uniform_list,
};
use language::{Anchor, BufferId, OutlineItem};
use project::{File, Project, ProjectEntryId, ProjectPath, WorktreeId};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use theme::ActiveTheme;
use ui::{Color, Icon, IconName, Label, prelude::*};
use util::{ResultExt, rel_path::RelPath};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    item::ItemHandle,
};

mod canvas_utils;
mod constants;
mod graph_engine;
mod layout_fruchterman_reingold;
mod quadtree;

use canvas_utils::{to_logical_pt, to_screen_pt, visible_logical_bounds};
use constants::*;

const UPDATE_DEBOUNCE: Duration = Duration::from_millis(50);
const GRAPH_LENS_PANEL_KEY: &str = "graph_lens_panel";

actions!(
    graph_lens,
    [
        ToggleFocus,
        Toggle,
        SelectNext,
        SelectPrevious,
        ToggleExpanded,
        OpenSelected,
        CollapseAllEntries,
        Rename,
        Duplicate,
        Delete,
        ToggleHideGitIgnore,
        ToggleHideHidden
    ]
);

pub trait OutlineProvider: Send + Sync {
    fn fetch_outlines(
        &self,
        buffer_id: BufferId,
        cx: &mut App,
    ) -> Option<Task<Vec<OutlineItem<Anchor>>>>;
}

pub struct EditorOutlineProvider {
    editor: gpui::WeakEntity<Editor>,
}

impl OutlineProvider for EditorOutlineProvider {
    fn fetch_outlines(
        &self,
        buffer_id: BufferId,
        cx: &mut App,
    ) -> Option<Task<Vec<OutlineItem<Anchor>>>> {
        self.editor
            .upgrade()
            .map(|e| e.update(cx, |editor, cx| editor.buffer_outline_items(buffer_id, cx)))
    }
}

pub struct TreeBuilder;

impl TreeBuilder {
    pub fn build(
        visible_worktrees: Vec<(WorktreeId, worktree::Snapshot)>,
        open_files: HashMap<ProjectEntryId, BufferId>,
        open_outlines: HashMap<ProjectEntryId, Vec<OutlineItem<Anchor>>>,
        expanded_dirs: HashSet<(WorktreeId, ProjectEntryId)>,
        expanded_files: HashSet<(WorktreeId, ProjectEntryId)>,
    ) -> (Vec<CachedEntry>, SharedString) {
        let mut entries = Vec::new();
        let mut current_id = 0;
        let mut project_title = String::new();

        for (worktree_id, snapshot) in visible_worktrees {
            if let Some(root) = snapshot.root_entry() {
                if project_title.is_empty() {
                    project_title = root.path.file_name().unwrap_or_default().to_string();
                }

                let mut children: Vec<_> = snapshot.child_entries(&root.path).cloned().collect();
                children.reverse();

                let mut stack = Vec::new();
                for child in children {
                    stack.push((child, 0));
                }

                while let Some((entry, depth)) = stack.pop() {
                    let is_dir = entry.is_dir();
                    let is_open = !is_dir && open_files.contains_key(&entry.id);

                    let is_expanded = if is_dir {
                        expanded_dirs.contains(&(worktree_id, entry.id))
                    } else {
                        is_open && expanded_files.contains(&(worktree_id, entry.id))
                    };

                    let kind = if is_dir {
                        LensEntryKind::Dir
                    } else {
                        LensEntryKind::File
                    };

                    let name =
                        SharedString::from(entry.path.file_name().unwrap_or_default().to_string());

                    entries.push(CachedEntry {
                        id: current_id,
                        worktree_id,
                        entry_id: Some(entry.id),
                        path: entry.path.clone(),
                        kind: kind.clone(),
                        name,
                        depth,
                        is_expanded,
                        is_open,
                    });
                    current_id += 1;

                    if is_expanded {
                        if is_dir {
                            let mut children: Vec<_> =
                                snapshot.child_entries(&entry.path).cloned().collect();
                            children.reverse();
                            for child in children {
                                stack.push((child, depth + 1));
                            }
                        } else if is_open {
                            if let Some(outlines) = open_outlines.get(&entry.id) {
                                for outline in outlines {
                                    let symbol_name = SharedString::from(outline.text.clone());
                                    entries.push(CachedEntry {
                                        id: current_id,
                                        worktree_id,
                                        entry_id: None,
                                        path: entry.path.clone(),
                                        kind: LensEntryKind::Outline(symbol_name.clone()),
                                        name: symbol_name,
                                        depth: depth + 1 + outline.depth,
                                        is_expanded: false,
                                        is_open: false,
                                    });
                                    current_id += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        if project_title.is_empty() {
            project_title = "Workspace".to_string();
        }

        (entries, SharedString::from(project_title))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum LensEntryKind {
    Dir,
    File,
    Outline(SharedString),
}

#[derive(Clone)]
pub struct CachedEntry {
    pub id: usize,
    pub worktree_id: WorktreeId,
    pub entry_id: Option<ProjectEntryId>,
    pub path: Arc<RelPath>,
    pub kind: LensEntryKind,
    pub name: SharedString,
    pub depth: usize,
    pub is_expanded: bool,
    pub is_open: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    List,
    Graph,
}

struct State {
    project_name: SharedString,
    entries: Vec<CachedEntry>,
    expanded_dirs: HashSet<(WorktreeId, ProjectEntryId)>,
    expanded_files: HashSet<(WorktreeId, ProjectEntryId)>,
    selected_index: Option<usize>,
    active_tab: ActiveTab,
    graph_engine: graph_engine::GraphEngine,

    // Camera and Interaction State
    zoom: f32,
    pan: Point<f32>,
    viewport_origin: Point<Pixels>,
    viewport_size: Size<Pixels>,
    is_panning: bool,
    pan_last_pos: Option<Point<f32>>,
    dragging_node: Option<graph_engine::NodeId>,
    drag_offset: Option<Point<f32>>,
}

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, window: Option<&mut Window>, cx: &mut Context<Workspace>| {
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                workspace.toggle_panel_focus::<GraphLensPanel>(window, cx);
            });

            workspace.register_action(|workspace, _: &Toggle, window, cx| {
                if !workspace.toggle_panel_focus::<GraphLensPanel>(window, cx) {
                    workspace.close_panel::<GraphLensPanel>(window, cx);
                }
            });

            if let Some(window) = window {
                let graph_lens_panel = GraphLensPanel::new(workspace, window, cx);
                workspace.add_panel(graph_lens_panel, window, cx);
            }
        },
    )
    .detach();
}

pub struct GraphLensPanel {
    project: Entity<Project>,
    workspace: gpui::WeakEntity<Workspace>,
    outline_provider: Option<Arc<dyn OutlineProvider>>,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
    state: State,
    update_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl GraphLensPanel {
    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let workspace_handle = cx.entity().downgrade();

        cx.new(|cx| {
            let focus_handle = cx.focus_handle();

            let focus_subscription =
                cx.on_focus(&focus_handle, window, |_: &mut Self, _window, cx| {
                    cx.notify()
                });

            let workspace_subscription = cx.subscribe_in(
                &workspace_handle.upgrade().expect("Workspace must exist"),
                window,
                move |lens_panel, workspace, event, window, cx| {
                    if let workspace::Event::ActiveItemChanged = event {
                        if let Some((new_active_item, new_active_editor)) =
                            workspace_active_editor(workspace.read(cx), cx)
                        {
                            lens_panel.replace_active_editor(
                                new_active_item,
                                new_active_editor,
                                window,
                                cx,
                            );
                        } else {
                            lens_panel.clear_active_editor(window, cx);
                            cx.notify();
                        }
                    }
                },
            );

            let project_subscription = cx.subscribe_in(
                &project,
                window,
                |lens_panel, _project, event, window, cx| match event {
                    project::Event::WorktreeRemoved(id) => {
                        lens_panel
                            .state
                            .expanded_dirs
                            .retain(|(w_id, _)| w_id != id);
                        lens_panel
                            .state
                            .expanded_files
                            .retain(|(w_id, _)| w_id != id);
                        lens_panel.update_entries(None, window, cx);
                        cx.notify();
                    }
                    project::Event::WorktreeUpdatedEntries(_, _)
                    | project::Event::WorktreeAdded(_)
                    | project::Event::WorktreeOrderChanged => {
                        lens_panel.update_entries(Some(UPDATE_DEBOUNCE), window, cx);
                        cx.notify();
                    }
                    _ => {}
                },
            );

            let mut lens_panel = Self {
                project,
                workspace: workspace_handle,
                outline_provider: None,
                focus_handle,
                scroll_handle: UniformListScrollHandle::new(),

                state: State {
                    project_name: SharedString::new("Loading..."),
                    entries: Vec::new(),
                    expanded_dirs: HashSet::new(),
                    expanded_files: HashSet::new(),
                    selected_index: None,
                    active_tab: ActiveTab::Graph,
                    graph_engine: graph_engine::GraphEngine::new(800.0, 600.0),
                    zoom: 1.0,
                    pan: Point::default(),
                    viewport_origin: Point::default(),
                    viewport_size: Size {
                        width: px(800.0),
                        height: px(600.0),
                    },
                    is_panning: false,
                    pan_last_pos: None,
                    dragging_node: None,
                    drag_offset: None,
                },

                update_task: None,
                _subscriptions: vec![
                    focus_subscription,
                    workspace_subscription,
                    project_subscription,
                ],
            };

            lens_panel.update_entries(None, window, cx);
            lens_panel
        })
    }

    fn sync_graph_engine(&mut self) {
        let bounds_x = self.state.graph_engine.bounds.x;
        let bounds_y = self.state.graph_engine.bounds.y;

        let old_engine = std::mem::replace(
            &mut self.state.graph_engine,
            graph_engine::GraphEngine::new(bounds_x, bounds_y),
        );

        let mut parent_stack = Vec::new();

        for entry in &self.state.entries {
            while parent_stack.len() > entry.depth {
                parent_stack.pop();
            }

            let parent_id = parent_stack.last().copied();

            let kind = match entry.kind {
                LensEntryKind::Dir => graph_engine::NodeKind::Directory,
                LensEntryKind::File => graph_engine::NodeKind::File,
                LensEntryKind::Outline(_) => graph_engine::NodeKind::Outline,
            };

            let node_id = self
                .state
                .graph_engine
                .add_node(entry.name.to_string(), kind, parent_id);

            // Restore position, velocity, and size if it existed
            if let Some(old_node) = old_engine
                .nodes
                .iter()
                .find(|n| n.label == entry.name.to_string())
            {
                let node = &mut self.state.graph_engine.nodes[node_id.0];
                node.position = old_node.position;
                node.velocity = old_node.velocity;
                node.size = old_node.size;
            }

            if let Some(pid) = parent_id {
                self.state.graph_engine.add_edge(pid, node_id);
            }

            parent_stack.push(node_id);
        }
    }

    fn set_tab(&mut self, tab: ActiveTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.active_tab != tab {
            self.state.active_tab = tab;
            if tab == ActiveTab::Graph {
                // Re-trigger layout so nodes move to their calculated positions
                self.run_layout(window, cx);
            }
            cx.notify();
        }
    }

    fn replace_active_editor(
        &mut self,
        _item: Box<dyn ItemHandle>,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.outline_provider = Some(Arc::new(EditorOutlineProvider {
            editor: editor.downgrade(),
        }));
        self.update_entries(Some(UPDATE_DEBOUNCE), window, cx);
    }

    fn clear_active_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.outline_provider = None;
        self.state.expanded_files.clear();
        self.update_entries(None, window, cx);
    }

    fn update_entries(
        &mut self,
        debounce: Option<Duration>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = self.project.read(cx);
        let visible_worktrees: Vec<_> = project
            .visible_worktrees(cx)
            .map(|w| {
                let w = w.read(cx);
                (w.id(), w.snapshot())
            })
            .collect();

        let open_files: HashMap<ProjectEntryId, BufferId> = project
            .buffer_store()
            .read(cx)
            .buffers()
            .filter_map(|buffer| {
                let file = File::from_dyn(buffer.read(cx).file())?;
                Some((file.project_entry_id()?, buffer.read(cx).remote_id()))
            })
            .collect();

        let mut outline_tasks = HashMap::new();
        if let Some(provider) = &self.outline_provider {
            for (&entry_id, &buffer_id) in &open_files {
                if let Some(task) = provider.fetch_outlines(buffer_id, cx) {
                    outline_tasks.insert(entry_id, task);
                }
            }
        }

        let expanded_dirs = self.state.expanded_dirs.clone();
        let expanded_files = self.state.expanded_files.clone();

        self.update_task = Some(cx.spawn(async move |this, cx| {
            if let Some(duration) = debounce {
                cx.background_executor().timer(duration).await;
            }

            let mut open_outlines = HashMap::new();
            for (entry_id, task) in outline_tasks {
                open_outlines.insert(entry_id, task.await);
            }

            let (new_entries, new_title) = cx
                .background_executor()
                .spawn(async move {
                    TreeBuilder::build(
                        visible_worktrees,
                        open_files,
                        open_outlines,
                        expanded_dirs,
                        expanded_files,
                    )
                })
                .await;

            this.update(cx, |this, cx| {
                this.state.entries = new_entries;
                this.state.project_name = new_title;

                if let Some(idx) = this.state.selected_index {
                    this.state.selected_index =
                        Some(idx.min(this.state.entries.len().saturating_sub(1)));
                }
                this.sync_graph_engine();
                cx.notify();
            })
            .log_err();
        }));
    }

    fn toggle_expanded_state(
        &mut self,
        _: &ToggleExpanded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.state.selected_index else {
            return;
        };
        let Some(entry) = self.state.entries.get(idx).cloned() else {
            return;
        };
        let Some(entry_id) = entry.entry_id else {
            return;
        };

        match entry.kind {
            LensEntryKind::Dir => {
                if entry.is_expanded {
                    self.state
                        .expanded_dirs
                        .remove(&(entry.worktree_id, entry_id));
                } else {
                    self.state
                        .expanded_dirs
                        .insert((entry.worktree_id, entry_id));
                    self.project.update(cx, |p, cx| {
                        if let Some(task) = p.expand_entry(entry.worktree_id, entry_id, cx) {
                            task.detach_and_log_err(cx);
                        }
                    });
                }
            }
            LensEntryKind::File => {
                if let Some(workspace) = self.workspace.upgrade() {
                    workspace.update(cx, |ws, cx| {
                        ws.open_path_preview(
                            ProjectPath {
                                worktree_id: entry.worktree_id,
                                path: entry.path.clone(),
                            },
                            None,
                            true,
                            false,
                            true,
                            window,
                            cx,
                        )
                        .detach_and_log_err(cx);
                    });
                }
            }
            LensEntryKind::Outline(_) => return,
        }
        self.update_entries(None, window, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        if self.state.entries.is_empty() {
            return;
        }
        let current = self.state.selected_index.unwrap_or(0);
        let next = (current + 1).min(self.state.entries.len() - 1);
        self.state.selected_index = Some(next);
        self.scroll_handle
            .scroll_to_item(next, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn select_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.entries.is_empty() {
            return;
        }
        let current = self.state.selected_index.unwrap_or(0);
        let prev = current.saturating_sub(1);
        self.state.selected_index = Some(prev);
        self.scroll_handle
            .scroll_to_item(prev, gpui::ScrollStrategy::Bottom);
        cx.notify();
    }

    fn run_layout(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.state.graph_engine.bounds;
        let node_count = self.state.graph_engine.active_nodes.len();

        let mut simulator =
            layout_fruchterman_reingold::FruchtermanReingold::new(node_count, bounds.x, bounds.y);

        cx.spawn(async move |this, cx| {
            loop {
                if simulator.temp < MIN_TEMPERATURE {
                    break;
                }

                let is_alive = this.update(cx, |this, cx| {
                    simulator.update(&mut this.state.graph_engine);
                    cx.notify();
                });

                if is_alive.is_err() {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(SIMULATION_POLL_INTERVAL_MS))
                    .await;
            }
        })
        .detach();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .px_3()
            .py_2()
            .bg(cx.theme().colors().background)
            .border_b_1()
            .border_color(gpui::rgba(0x333333ff))
            .child(Label::new(format!("Project: {}", self.state.project_name)))
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .w_full()
            .border_b_1()
            .border_color(gpui::rgba(0x333333ff))
            .child(self.render_tab("List", ActiveTab::List, cx))
            .child(self.render_tab("Graph", ActiveTab::Graph, cx))
    }

    fn render_tab(
        &self,
        title: &'static str,
        tab: ActiveTab,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.state.active_tab == tab;
        div()
            .id(title)
            .cursor_pointer()
            .px_4()
            .py_1()
            .bg(if is_active {
                gpui::rgba(0x444444ff)
            } else {
                gpui::rgba(0x00000000)
            })
            .on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)))
            .child(Label::new(title))
    }

    fn render_content(
        &self,
        is_focused: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match self.state.active_tab {
            ActiveTab::List => self.render_list_view(is_focused, cx).into_any_element(),
            ActiveTab::Graph => self.render_graph_view(cx).into_any_element(),
        }
    }

    fn render_list_view(&self, is_focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        uniform_list(
            "graph_lens_entries",
            self.state.entries.len(),
            cx.processor(move |this, range, _window, cx| {
                let mut items = Vec::new();
                for ix in range {
                    let Some(entry) = this.state.entries.get(ix) else {
                        continue;
                    };
                    let is_selected = this.state.selected_index == Some(ix);
                    let indent = px((entry.depth * 16) as f32);

                    let (icon, icon_color) = match &entry.kind {
                        LensEntryKind::Dir => {
                            if entry.is_expanded {
                                (IconName::FolderOpen, Color::Muted)
                            } else {
                                (IconName::Folder, Color::Muted)
                            }
                        }
                        LensEntryKind::File => (IconName::File, Color::Default),
                        LensEntryKind::Outline(_) => (IconName::Code, Color::Accent),
                    };

                    let element = div()
                        .id(entry.id)
                        .pl(indent)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(is_selected && is_focused, |s| s.bg(gpui::rgba(0x444444ff)))
                        .when(is_selected && !is_focused, |s| s.bg(gpui::rgba(0x222222ff)))
                        .when(!is_selected, |s| s.hover(|s| s.bg(gpui::rgba(0x333333ff))))
                        .on_click(cx.listener({
                            let idx = ix;
                            move |this, _, window, cx| {
                                this.state.selected_index = Some(idx);
                                this.toggle_expanded_state(&ToggleExpanded, window, cx);
                            }
                        }))
                        .child(Icon::new(icon).color(icon_color).size(IconSize::Small))
                        .child(Label::new(entry.name.clone()).single_line());

                    items.push(element.into_any_element());
                }
                items
            }),
        )
        .track_scroll(&self.scroll_handle)
        .flex_1()
    }

    fn render_graph_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom = self.state.zoom;
        let pan = self.state.pan;
        let viewport_origin = self.state.viewport_origin;
        // Fix: Access viewport_size from the state struct
        let viewport_size = self.state.viewport_size;

        let mut viewport_bounds = visible_logical_bounds(viewport_origin, viewport_size, zoom, pan);
        // Expand bounds slightly to prevent aggressive culling at the edges
        viewport_bounds.origin.x -= 100.0;
        viewport_bounds.origin.y -= 100.0;
        viewport_bounds.size.width += 200.0;
        viewport_bounds.size.height += 200.0;

        // Clones for the main scope
        let active_nodes = self.state.graph_engine.active_nodes.clone();
        let edges = self.state.graph_engine.edges.clone();
        let nodes = self.state.graph_engine.nodes.clone();
        let entries = self.state.entries.clone();

        let view_handle = cx.entity().clone();

        // Secondary clones specifically for the move closure inside gpui::canvas
        let canvas_active_nodes = active_nodes.clone();
        let canvas_edges = edges.clone();
        let canvas_nodes = nodes.clone();

        let mut canvas_area = div()
            .id("canvas-area")
            .size_full()
            .absolute()
            .top_0()
            .left_0()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(1.0)).y;
                let zoom_speed = 0.001;
                let old_zoom = this.state.zoom;

                let delta_f32: f32 = delta.into();
                this.state.zoom *= 1.0 + (delta_f32 * zoom_speed);
                this.state.zoom = this.state.zoom.clamp(0.1, 5.0);

                let logical_mouse = to_logical_pt(
                    event.position,
                    this.state.viewport_origin,
                    old_zoom,
                    this.state.pan,
                );
                this.state.pan.x -= logical_mouse.x * (this.state.zoom - old_zoom);
                this.state.pan.y -= logical_mouse.y * (this.state.zoom - old_zoom);

                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    this.state.is_panning = true;

                    this.state.pan_last_pos = Some(Point {
                        x: event.position.x.into(),
                        y: event.position.y.into(),
                    });
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.state.is_panning {
                    if let Some(last_pos) = this.state.pan_last_pos {
                        let current_x: f32 = event.position.x.into();
                        let current_y: f32 = event.position.y.into();
                        this.state.pan.x += current_x - last_pos.x;
                        this.state.pan.y += current_y - last_pos.y;
                        this.state.pan_last_pos = Some(Point {
                            x: current_x,
                            y: current_y,
                        });
                        cx.notify();
                    }
                } else if let Some(offset) = this.state.drag_offset {
                    if let Some(id) = this.state.dragging_node {
                        let logical_pt = to_logical_pt(
                            event.position,
                            this.state.viewport_origin,
                            this.state.zoom,
                            this.state.pan,
                        );
                        let node = &mut this.state.graph_engine.nodes[id.0];
                        node.position.x = logical_pt.x - offset.x;
                        node.position.y = logical_pt.y - offset.y;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(id) = this.state.dragging_node {
                        this.state.graph_engine.nodes[id.0].is_fixed = false;
                    }
                    this.state.dragging_node = None;
                    this.state.drag_offset = None;
                    this.state.is_panning = false;
                    this.state.pan_last_pos = None;
                    cx.notify();
                }),
            );

        // Render Edges via Canvas
        canvas_area = canvas_area.child(
            gpui::canvas(
                move |bounds, _, _| bounds,
                move |bounds, _bounds_data, window, cx| {
                    let view_handle = view_handle.clone();
                    let new_origin = bounds.origin;
                    let new_size = bounds.size;

                    window.on_next_frame(move |_window, cx| {
                        // Fix: Added _window and cx arguments
                        view_handle.update(cx, |this, cx| {
                            if this.state.viewport_origin != new_origin
                                || this.state.viewport_size != new_size
                            {
                                this.state.viewport_origin = new_origin;
                                this.state.viewport_size = new_size;
                                cx.notify();
                            }
                        });
                    });

                    for edge in &canvas_edges {
                        if canvas_active_nodes.contains(&edge.source)
                            && canvas_active_nodes.contains(&edge.target)
                        {
                            let n1 = &canvas_nodes[edge.source.0];
                            let n2 = &canvas_nodes[edge.target.0];

                            let center1 = Point {
                                x: n1.position.x + n1.size.width / 2.0,
                                y: n1.position.y + n1.size.height / 2.0,
                            };
                            let center2 = Point {
                                x: n2.position.x + n2.size.width / 2.0,
                                y: n2.position.y + n2.size.height / 2.0,
                            };

                            let screen1 = to_screen_pt(center1, bounds.origin, zoom, pan);
                            let screen2 = to_screen_pt(center2, bounds.origin, zoom, pan);

                            let mut path = gpui::Path::new(screen1);
                            path.line_to(screen2);
                            window.paint_path(path, gpui::rgba(0x66666666));
                        }
                    }
                },
            )
            .absolute()
            .size_full(),
        );

        // Render Nodes as absolute elements (divs)
        for &node_id in &active_nodes {
            let node = &nodes[node_id.0];

            let node_bounds = Bounds {
                origin: node.position,
                size: node.size,
            };

            if viewport_bounds.intersects(&node_bounds) {
                let screen_pt = to_screen_pt(node.position, viewport_origin, zoom, pan);
                let screen_w = px(node.size.width * zoom);
                let screen_h = px(node.size.height * zoom);

                let entry_index = entries
                    .iter()
                    .position(|e| e.name.to_string() == node.label);
                let is_selected = entry_index.is_some() && self.state.selected_index == entry_index;

                let (bg_color, border_color): (gpui::Hsla, gpui::Hsla) = match node.kind {
                    graph_engine::NodeKind::Directory | graph_engine::NodeKind::Worktree => {
                        (gpui::rgba(0x4CAF5022).into(), gpui::rgba(0x4CAF50FF).into())
                    }
                    graph_engine::NodeKind::File => (
                        cx.theme().colors().element_background,
                        gpui::rgba(0x2196F3FF).into(),
                    ),
                    _ => (
                        cx.theme().colors().element_background,
                        gpui::rgba(0xFFC107FF).into(),
                    ),
                };

                let element = div()
                    .absolute()
                    .left(screen_pt.x)
                    .top(screen_pt.y)
                    .w(screen_w)
                    .h(screen_h)
                    .bg(bg_color)
                    .border(px(1.0 * zoom.max(0.5)))
                    .border_color(if is_selected {
                        cx.theme().colors().text_accent
                    } else {
                        border_color
                    })
                    .rounded(px(4.0 * zoom))
                    .p(px(4.0 * zoom))
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().colors().element_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener({
                            let n_id = node_id;
                            let e_idx = entry_index;
                            move |this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();

                                if let Some(idx) = e_idx {
                                    this.state.selected_index = Some(idx);
                                    this.toggle_expanded_state(&ToggleExpanded, window, cx);
                                }

                                this.state.dragging_node = Some(n_id);
                                this.state.graph_engine.nodes[n_id.0].is_fixed = true;

                                let logical_pt = to_logical_pt(
                                    event.position,
                                    this.state.viewport_origin,
                                    this.state.zoom,
                                    this.state.pan,
                                );
                                let node_pos = this.state.graph_engine.nodes[n_id.0].position;

                                this.state.drag_offset = Some(Point {
                                    x: logical_pt.x - node_pos.x,
                                    y: logical_pt.y - node_pos.y,
                                });
                                cx.notify();
                            }
                        }),
                    )
                    .child(Label::new(node.label.clone()).single_line());

                canvas_area = canvas_area.child(element);
            }
        }

        // Main Layout Container
        div()
            .flex_1()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1().size_full().child(canvas_area))
            .child(
                div()
                    .w_full()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(gpui::rgba(0x333333ff))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                div()
                                    .id("btn-zoom-in")
                                    .cursor_pointer()
                                    .px_2()
                                    .py_1()
                                    .bg(cx.theme().colors().element_background)
                                    .hover(|s| s.opacity(0.8))
                                    .rounded_md()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.zoom *= 1.1;
                                        cx.notify();
                                    }))
                                    .child(Label::new("Zoom In")),
                            )
                            .child(
                                div()
                                    .id("btn-zoom-out")
                                    .cursor_pointer()
                                    .px_2()
                                    .py_1()
                                    .bg(cx.theme().colors().element_background)
                                    .hover(|s| s.opacity(0.8))
                                    .rounded_md()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.zoom *= 0.9;
                                        cx.notify();
                                    }))
                                    .child(Label::new("Zoom Out")),
                            ),
                    )
                    .child(
                        div()
                            .id("btn-run-layout")
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .bg(cx.theme().colors().element_background)
                            .hover(|s| s.opacity(0.8))
                            .rounded_md()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.run_layout(window, cx)),
                            )
                            .child(Label::new("Run Layout")),
                    ),
            )
    }
}

impl EventEmitter<PanelEvent> for GraphLensPanel {}

impl Focusable for GraphLensPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GraphLensPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_focused = self.focus_handle.is_focused(window);

        let mut dispatch_context = KeyContext::new_with_defaults();
        dispatch_context.add("GraphLensPanel");
        dispatch_context.add("menu");
        if is_focused {
            dispatch_context.add("GraphLensPanelFocused");
        }

        div()
            .id("graph-lens-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .key_context(dispatch_context)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::toggle_expanded_state))
            .child(self.render_header(cx))
            .child(self.render_tab_bar(cx))
            .child(self.render_content(is_focused, window, cx))
    }
}

impl Panel for GraphLensPanel {
    fn persistent_name() -> &'static str {
        "GraphLensPanel"
    }
    fn panel_key() -> &'static str {
        GRAPH_LENS_PANEL_KEY
    }
    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        DockPosition::Right
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Right | DockPosition::Left)
    }
    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(360.)
    }
    fn activation_priority(&self) -> u32 {
        1000
    }
    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        false
    }
    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }
    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::ListTree)
    }
    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Graph Lens")
    }
    fn set_position(
        &mut self,
        _position: DockPosition,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
    fn set_active(&mut self, _active: bool, _window: &mut Window, _cx: &mut Context<Self>) {}
}

fn workspace_active_editor(
    workspace: &Workspace,
    cx: &App,
) -> Option<(Box<dyn ItemHandle>, Entity<Editor>)> {
    let active_item = workspace.active_item(cx)?;
    let active_editor = active_item
        .act_as::<Editor>(cx)
        .filter(|editor| editor.read(cx).mode().is_full())?;
    Some((active_item, active_editor))
}
