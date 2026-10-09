//! Window root: title bar, sidebar and the routed main area. Owned by the shell workstream.

use gpui_kit::*;

use crate::state::AppState;

pub struct Workspace {
    state: Entity<AppState>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        Self { state, _subscriptions: subscriptions }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let videos = self.state.read(cx).rows().len();
        div().size_full().child(format!("VideoCRM — {videos} videos"))
    }
}
