//! Tooltips that explain a passing state, such as why a control is disabled.

use gpui_kit::component::h_flex;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Div, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, TestSupportExt as _, div,
};

/// `control` with a tooltip that says why it is disabled, while `reason` is
/// `Some`. A kit control's own tooltip is hidden only when the pointer leaves
/// the control, so one dropped while the pointer rests on it (the state
/// changed, say an upload ended) stays on screen, also over other pages. This
/// one is drawn with every frame: it leaves with the state that explains it,
/// or with the control.
pub fn state_tooltip(
    id: impl Into<ElementId>,
    reason: Option<SharedString>,
    control: impl IntoElement,
) -> Stateful<Div> {
    h_flex().id(id).child(control).when_some(reason, |this, reason| {
        this.tooltip(move |window, cx| {
            let reason = reason.clone();
            // Tests look for the tooltip on screen.
            Tooltip::element(move |_, _| div().id("state-tooltip").test_support().child(reason.clone()))
                .build(window, cx)
        })
    })
}
