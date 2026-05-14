use editor::Editor;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyContext, ParentElement, Render, Styled, Subscription, Task, UniformListScrollHandle, Window,
    actions, div, px, uniform_list,
};
use language::{Anchor, BufferId, OutlineItem};
use project::{File, Project, ProjectEntryId, ProjectPath, WorktreeId};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use theme;
use ui::{Color, Icon, IconName, Label, prelude::*};
use util::{ResultExt, rel_path::RelPath};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    item::ItemHandle,
};

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

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, window: Option<&mut Window>, cx: &mut Context<Workspace>| {
            // 1. Core Panel Toggles (Shared across Outline & Project)
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                workspace.toggle_panel_focus::<GraphLensPanel>(window, cx);
            });

            workspace.register_action(|workspace, _: &Toggle, window, cx| {
                if !workspace.toggle_panel_focus::<GraphLensPanel>(window, cx) {
                    workspace.close_panel::<GraphLensPanel>(window, cx);
                }
            });

            // 2. Tree Manipulation Actions
            workspace.register_action(|workspace, action: &CollapseAllEntries, window, cx| {
                if let Some(panel) = workspace.panel::<GraphLensPanel>(cx) {
                    panel.update(cx, |panel, cx| {
                        panel.collapse_all_entries(action, window, cx);
                    });
                }
            });

            // 3. File System / Node Mutations (Inherited from ProjectPanel)
            workspace.register_action(|workspace, action: &Rename, window, cx| {
                workspace.open_panel::<GraphLensPanel>(window, cx);
                if let Some(panel) = workspace.panel::<GraphLensPanel>(cx) {
                    panel.update(cx, |panel, cx| {
                        if let Some(first_marked) = panel.state.selected_index {
                            panel.state.selected_index = Some(first_marked);
                        }
                        panel.rename(action, window, cx);
                    });
                }
            });

            workspace.register_action(|workspace, action: &Duplicate, window, cx| {
                workspace.open_panel::<GraphLensPanel>(window, cx);
                if let Some(panel) = workspace.panel::<GraphLensPanel>(cx) {
                    panel.update(cx, |panel, cx| {
                        panel.duplicate(action, window, cx);
                    });
                }
            });

            workspace.register_action(|workspace, action: &Delete, window, cx| {
                if let Some(panel) = workspace.panel::<GraphLensPanel>(cx) {
                    panel.update(cx, |panel, cx| panel.delete(action, window, cx));
                }
            });

            // 4. View Configuration Actions
            workspace.register_action(|workspace, _: &ToggleHideGitIgnore, _, cx| {
                let fs = workspace.app_state().fs.clone();
                settings::update_settings_file(fs, cx, move |setting, _| {
                    setting.project_panel.get_or_insert_default().hide_gitignore = Some(
                        !setting
                            .project_panel
                            .get_or_insert_default()
                            .hide_gitignore
                            .unwrap_or(false),
                    );
                })
            });

            workspace.register_action(|workspace, _: &ToggleHideHidden, _, cx| {
                let fs = workspace.app_state().fs.clone();
                settings::update_settings_file(fs, cx, move |setting, _| {
                    setting.project_panel.get_or_insert_default().hide_hidden = Some(
                        !setting
                            .project_panel
                            .get_or_insert_default()
                            .hide_hidden
                            .unwrap_or(false),
                    );
                })
            });

            if let Some(window) = window {
                let graph_lens_panel = GraphLensPanel::new(workspace, window, cx);
                workspace.add_panel(graph_lens_panel, window, cx);
            }
        },
    )
    .detach();
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

struct State {
    project_name: SharedString,
    entries: Vec<CachedEntry>,
    expanded_dirs: HashSet<(WorktreeId, ProjectEntryId)>,
    expanded_files: HashSet<(WorktreeId, ProjectEntryId)>,
    selected_index: Option<usize>,
}

pub struct GraphLensPanel {
    project: Entity<Project>,
    workspace: gpui::WeakEntity<Workspace>,
    active_editor: Option<gpui::WeakEntity<Editor>>,
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

            // 1. Focus Subscription
            let focus_subscription =
                cx.on_focus(&focus_handle, window, |_: &mut Self, _window, cx| {
                    cx.notify()
                });

            // 2. Workspace Subscription (Outline Paradigm)
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

            // 3. Project Subscription (Project Explorer Paradigm)
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
                    project::Event::ExpandedAllForEntry(worktree_id, entry_id) => {
                        lens_panel.expand_all_for_entry(*worktree_id, *entry_id, cx);
                        lens_panel.update_entries(None, window, cx);
                        cx.notify();
                    }
                    _ => {}
                },
            );

            // 4. Global UI Subscriptions (Icons and Settings)
            let icons_subscription = cx.observe_global::<file_icons::FileIcons>(|_, cx| {
                cx.notify();
            });

            // Initialize the State & Entity
            let mut lens_panel = Self {
                project: project.clone(),
                workspace: workspace_handle,
                active_editor: None,
                focus_handle,
                scroll_handle: UniformListScrollHandle::new(),

                state: State {
                    project_name: SharedString::new("Project Not Found"),
                    entries: Vec::new(),
                    expanded_dirs: HashSet::new(),
                    expanded_files: HashSet::new(),
                    selected_index: None,
                },

                update_task: None,
                _subscriptions: vec![
                    focus_subscription,
                    workspace_subscription,
                    project_subscription,
                    icons_subscription,
                ],
            };

            // 5. Initial Bootstrapping
            lens_panel.update_entries(None, window, cx);

            if let Some((item, editor)) = workspace_active_editor(workspace, cx) {
                lens_panel.replace_active_editor(item, editor, window, cx);
            }

            lens_panel
        })
    }

    fn replace_active_editor(
        &mut self,
        _item: Box<dyn ItemHandle>,
        editor: Entity<Editor>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_editor = Some(editor.downgrade());
        self.update_entries(Some(UPDATE_DEBOUNCE), _window, cx);
    }

    fn clear_active_editor(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.active_editor = None;
        self.state.expanded_files.clear();
        self.update_entries(None, _window, cx);
    }

    fn expand_all_for_entry(
        &mut self,
        _worktree_id: WorktreeId,
        _entry_id: ProjectEntryId,
        _cx: &mut Context<Self>,
    ) {
        // Implementation for deep tree expansion goes here
    }

    pub fn collapse_all_entries(
        &mut self,
        _: &CollapseAllEntries,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.expanded_dirs.clear();
        self.state.expanded_files.clear();
        self.update_entries(None, window, cx);
        cx.notify();
    }

    pub fn rename(&mut self, _: &Rename, _window: &mut Window, _cx: &mut Context<Self>) {
        // Delegate to inline editor state
    }

    pub fn duplicate(&mut self, _: &Duplicate, _window: &mut Window, _cx: &mut Context<Self>) {
        // Delegate to underlying project commands
    }

    pub fn delete(&mut self, _: &Delete, _window: &mut Window, _cx: &mut Context<Self>) {
        // Delegate to underlying project commands
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

        // 1. Gather all currently open files
        let open_files: HashMap<ProjectEntryId, BufferId> = project
            .buffer_store()
            .read(cx)
            .buffers()
            .filter_map(|buffer| {
                let file = File::from_dyn(buffer.read(cx).file())?;
                Some((file.project_entry_id()?, buffer.read(cx).remote_id()))
            })
            .collect();

        // 2. Start fetching outlines for these buffers via the active editor
        let mut outline_tasks = HashMap::new();
        if let Some(editor) = self.active_editor.as_ref().and_then(|e| e.upgrade()) {
            for (&entry_id, &buffer_id) in &open_files {
                let task = editor.update(cx, |e, cx| e.buffer_outline_items(buffer_id, cx));
                outline_tasks.insert(entry_id, task);
            }
        }

        let expanded_dirs = self.state.expanded_dirs.clone();
        let expanded_files = self.state.expanded_files.clone();

        self.update_task = Some(cx.spawn(async move |this, cx| {
            if let Some(duration) = debounce {
                cx.background_executor().timer(duration).await;
            }

            // 3. Await all outline tasks in the background
            let mut open_outlines: HashMap<ProjectEntryId, Vec<OutlineItem<Anchor>>> =
                HashMap::new();
            for (entry_id, task) in outline_tasks {
                open_outlines.insert(entry_id, task.await);
            }

            // 4. Build the tree
            let (new_entries, new_title) = cx
                .background_executor()
                .spawn(async move {
                    let mut entries = Vec::new();
                    let mut current_id = 0;
                    let mut project_title = String::new();

                    for (worktree_id, snapshot) in visible_worktrees {
                        if let Some(root) = snapshot.root_entry() {
                            if project_title.is_empty() {
                                project_title =
                                    root.path.file_name().unwrap_or_default().to_string();
                            }

                            let mut children: Vec<_> =
                                snapshot.child_entries(&root.path).cloned().collect();
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

                                let name = SharedString::from(
                                    entry.path.file_name().unwrap_or_default().to_string(),
                                );

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
                                        // 5. Inject the actual outline elements instead of a dummy node
                                        if let Some(outlines) = open_outlines.get(&entry.id) {
                                            for outline in outlines {
                                                let symbol_name =
                                                    SharedString::from(outline.text.clone());
                                                entries.push(CachedEntry {
                                                    id: current_id,
                                                    worktree_id,
                                                    entry_id: None,
                                                    path: entry.path.clone(),
                                                    kind: LensEntryKind::Outline(
                                                        symbol_name.clone(),
                                                    ),
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
                })
                .await;

            this.update(cx, |this, cx| {
                this.state.entries = new_entries;
                this.state.project_name = new_title;

                if let Some(idx) = this.state.selected_index {
                    this.state.selected_index =
                        Some(idx.min(this.state.entries.len().saturating_sub(1)));
                }

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
                if entry.is_open {
                    // IF OPEN: Toggle its outline children
                    if entry.is_expanded {
                        self.state
                            .expanded_files
                            .remove(&(entry.worktree_id, entry_id));
                    } else {
                        self.state
                            .expanded_files
                            .insert((entry.worktree_id, entry_id));
                    }
                } else {
                    // IF CLOSED: Open it in the editor
                    if let Some(workspace) = self.workspace.upgrade() {
                        workspace.update(cx, |ws, cx| {
                            ws.open_path_preview(
                                ProjectPath {
                                    worktree_id: entry.worktree_id,
                                    path: entry.path.clone(),
                                },
                                None,
                                true,  // Focus the opened item
                                false, // Not a preview
                                true,  // Make visible
                                window,
                                cx,
                            )
                            .detach_and_log_err(cx);
                        });
                    }
                }
            }
            LensEntryKind::Outline(_) => return, // Handle clicking an outline node if desired
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

    fn dispatch_context(&self, window: &Window, _cx: &Context<Self>) -> KeyContext {
        let mut dispatch_context = KeyContext::new_with_defaults();
        dispatch_context.add("GraphLensPanel");
        dispatch_context.add("menu");
        if self.focus_handle.is_focused(window) {
            dispatch_context.add("GraphLensPanelFocused");
        }
        dispatch_context
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

        div()
            .id("graph-lens-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .key_context(self.dispatch_context(window, cx))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::toggle_expanded_state))
            // --- NEW TITLE BAR ---
            .child(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .bg(cx.theme().colors().background) // subtle background for the header
                    .border_b_1()
                    .border_color(gpui::rgba(0x333333ff)) // subtle border
                    .child(Label::new(format!("Project: {}", self.state.project_name))),
            )
            // ---------------------
            .child(
                uniform_list("graph_lens_entries", self.state.entries.len(), {
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
                    })
                })
                .track_scroll(&self.scroll_handle)
                .flex_1(),
            )
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
