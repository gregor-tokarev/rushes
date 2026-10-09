//! Settings geometry shared by the Settings page and onboarding.
//!
//! The Settings page lays out every section with the kit's
//! [`gpui_kit::component::setting`] groups and items: a muted group title, the
//! label and description on the leading side and the control trailing, stacked
//! below once the page is narrow. Long values (paths, IDs, instructions) stack
//! below their label at any width. Onboarding shows some of the same fields
//! outside the Settings page; [`SettingField`] draws them with the same
//! typography.

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, Rems, RenderOnce, SharedString, Styled as _, Window, div,
};

/// The widest a settings text field grows: a path or an ID doesn't need the
/// whole page.
pub const FIELD_MAX_WIDTH: Rems = Rems(36.);

/// A stacked settings field outside the Settings page: the label and its
/// description, then the control, as the kit draws a vertical setting item.
#[derive(IntoElement)]
pub struct SettingField {
    label: SharedString,
    description: Option<SharedString>,
    control: Option<AnyElement>,
}

impl SettingField {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), description: None, control: None }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }
}

impl RenderOnce for SettingField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_3()
            .child(
                v_flex().w_full().child(div().text_sm().child(self.label)).when_some(self.description, |this, text| {
                    this.child(div().text_sm().text_color(cx.theme().muted_foreground).child(text))
                }),
            )
            .children(self.control.map(|control| div().w_full().max_w(FIELD_MAX_WIDTH).child(control)))
    }
}
