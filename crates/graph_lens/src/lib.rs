use gpui::{
    Action, App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, Pixels, Render,
    Window, actions, px,
};
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

            // 1. Create the panel entity
            let panel = cx.new(|cx| GraphLensPanel::new(cx));

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

pub struct GraphLensPanel {
    focus_handle: FocusHandle,
}

impl GraphLensPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
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
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(Label::new("Hello World: Graph Lens Foundation"))
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
