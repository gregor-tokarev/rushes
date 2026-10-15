//! Settings sections shared by the Settings page and onboarding.
//!
//! The Settings page lays out every section with the kit's setting groups and
//! items: a muted group title, the label and description on the leading side
//! and the control trailing, stacked below once the page is narrow. Long values
//! (paths, IDs, instructions) stack below their label at any width. Groups
//! are [`FIELD_MAX_WIDTH`] wide at most, so trailing controls and stacked
//! fields end on one edge.
//!
//! A view whose settings also appear outside the Settings page (YouTube and
//! AI editing in onboarding) describes them once as [`SettingsSection`]s of
//! fields rendered by the view itself: [`SettingsSection::setting_group`] turns
//! a section into a kit group for the Settings page, [`SettingsSection::render`]
//! draws it in the same typography anywhere else.

use std::rc::Rc;

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::setting::{RenderOptions, SettingField, SettingGroup, SettingItem};
use gpui_kit::component::{ActiveTheme as _, AxisExt as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Axis, Context, Entity, IntoElement, ParentElement as _, Rems, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::menu_item;

/// The widest a settings group, and with it a text field, grows: a path or an
/// ID doesn't need the whole page, and a switch or select beside a short
/// label stays within reach of it.
pub const FIELD_MAX_WIDTH: Rems = Rems(40.);

/// A settings group on the column every group shares.
pub fn setting_group() -> SettingGroup {
    SettingGroup::new().max_w(FIELD_MAX_WIDTH)
}

/// The kit's `SettingField::dropdown`: a button naming the chosen option that
/// opens a menu of `options`, `(value, label)` pairs, with a check on the
/// chosen one. Its items are [`menu_item`]s, which grow with the zoom.
pub fn dropdown_field(
    options: Vec<(SharedString, SharedString)>,
    value: impl Fn(&App) -> SharedString + 'static,
    set_value: impl Fn(SharedString, &mut App) + 'static,
) -> SettingField<SharedString> {
    let options = Rc::new(options);
    let set_value = Rc::new(set_value);
    SettingField::render(move |field: &RenderOptions, _, cx| {
        let current = value(cx);
        let label =
            options.iter().find(|(value, _)| *value == current).map_or(current.clone(), |(_, label)| label.clone());
        let (options, set_value) = (options.clone(), set_value.clone());
        // The kit's id for the button, which tests click.
        Button::new("btn")
            .when(field.layout().is_vertical(), |this| this.w_full())
            .label(label)
            .dropdown_caret(true)
            .outline()
            .disabled(field.is_disabled())
            .with_size(field.size())
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                // Zoomed in, the options may be taller than the window.
                options.iter().fold(menu.scrollable(true), |menu, (value, label)| {
                    let (value, set_value) = (value.clone(), set_value.clone());
                    menu.item(
                        menu_item(label.clone())
                            .checked(value == current)
                            .on_click(move |_, _, cx| set_value(value.clone(), cx)),
                    )
                })
            })
    })
}

/// Renders one part of a settings view.
type Part<V> = Rc<dyn Fn(&mut V, &mut Window, &mut Context<V>) -> AnyElement>;

/// A labelled setting of a view: the label and description, and a control the
/// view renders.
pub struct SettingsField<V> {
    label: SharedString,
    description: Option<SharedString>,
    stacked: bool,
    control: Part<V>,
}

impl<V: 'static> SettingsField<V> {
    pub fn new<E: IntoElement>(
        label: impl Into<SharedString>,
        control: impl Fn(&mut V, &mut Window, &mut Context<V>) -> E + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            description: None,
            stacked: false,
            control: Rc::new(move |view, window, cx| control(view, window, cx).into_any_element()),
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Puts the control below the label at every width, at most
    /// [`FIELD_MAX_WIDTH`] wide: for text fields.
    pub fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }
}

enum Item<V> {
    Field(SettingsField<V>),
    Element(Part<V>),
}

/// A group of a view's settings: an optional muted title and description,
/// then its fields and free-form parts in order.
pub struct SettingsSection<V> {
    title: Option<SharedString>,
    description: Option<SharedString>,
    keywords: Vec<SharedString>,
    items: Vec<Item<V>>,
}

impl<V: 'static> Default for SettingsSection<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: 'static> SettingsSection<V> {
    pub fn new() -> Self {
        Self { title: None, description: None, keywords: Vec::new(), items: Vec::new() }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Words the Settings page search finds the section's items by, besides
    /// their labels and its title: free-form parts have no label of their own.
    pub fn keywords(mut self, keywords: impl IntoIterator<Item = &'static str>) -> Self {
        self.keywords.extend(keywords.into_iter().map(SharedString::from));
        self
    }

    /// Whether the section is titled `title`.
    pub fn has_title(&self, title: &str) -> bool {
        self.title.as_deref() == Some(title)
    }

    pub fn field(mut self, field: SettingsField<V>) -> Self {
        self.items.push(Item::Field(field));
        self
    }

    /// A full-width part without a label, such as a list or a status.
    pub fn element<E: IntoElement>(
        mut self,
        part: impl Fn(&mut V, &mut Window, &mut Context<V>) -> E + 'static,
    ) -> Self {
        self.items.push(Item::Element(Rc::new(move |view, window, cx| part(view, window, cx).into_any_element())));
        self
    }

    /// The section as a group of the kit's Settings page, rendering its parts
    /// through `view`. The page must re-render when `view` notifies.
    pub fn setting_group(self, view: &Entity<V>) -> SettingGroup {
        // The kit's search matches items, not group titles.
        let keywords: Vec<SharedString> = self.title.iter().cloned().chain(self.keywords).collect();
        let mut group = setting_group();
        if let Some(title) = self.title {
            group = group.title(title);
        }
        if let Some(description) = self.description {
            group = group.description(description);
        }
        group.items(self.items.into_iter().map(|item| match item {
            Item::Field(field) => {
                let (view, control, stacked) = (view.clone(), field.control, field.stacked);
                let control = SettingField::render(move |_, window, cx| {
                    let control = view.update(cx, |view, cx| control(view, window, cx));
                    if stacked { div().w_full().max_w(FIELD_MAX_WIDTH).child(control) } else { div().child(control) }
                });
                let mut item = SettingItem::new(field.label, control).keywords(keywords.clone());
                if let Some(description) = field.description {
                    item = item.description(description);
                }
                if stacked { item.layout(Axis::Vertical) } else { item }
            }
            Item::Element(part) => {
                let view = view.clone();
                SettingItem::render(move |_, window, cx| view.update(cx, |view, cx| part(view, window, cx)))
                    .keywords(keywords.clone())
            }
        }))
    }

    /// The section outside the Settings page, in the same typography; every
    /// field stacks, as on a narrow Settings page.
    pub fn render(&self, view: &mut V, window: &mut Window, cx: &mut Context<V>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let heading = (self.title.is_some() || self.description.is_some()).then(|| {
            v_flex()
                .gap_1()
                .children(self.title.clone().map(|title| div().text_color(muted).child(title)))
                .children(self.description.clone().map(|text| div().text_sm().text_color(muted).child(text)))
        });
        let items: Vec<AnyElement> = self
            .items
            .iter()
            .map(|item| match item {
                Item::Field(field) => FieldStack::new(field.label.clone())
                    .when_some(field.description.clone(), FieldStack::description)
                    .control((field.control)(view, window, cx))
                    .narrow(field.stacked)
                    .into_any_element(),
                Item::Element(part) => part(view, window, cx),
            })
            .collect();
        v_flex().w_full().gap_4().children(heading).children(items).into_any_element()
    }
}

/// A setting outside the Settings page: the label and its description, then
/// the control, as the kit draws a stacked setting item.
#[derive(IntoElement)]
pub struct FieldStack {
    label: SharedString,
    description: Option<SharedString>,
    control: Option<AnyElement>,
    narrow: bool,
}

impl FieldStack {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), description: None, control: None, narrow: true }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// Caps the control at [`FIELD_MAX_WIDTH`] (the default); off for
    /// controls that size themselves, such as switches and menus.
    pub fn narrow(mut self, narrow: bool) -> Self {
        self.narrow = narrow;
        self
    }
}

impl RenderOnce for FieldStack {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let control = self.control.map(|control| {
            if self.narrow {
                div().w_full().max_w(FIELD_MAX_WIDTH).child(control).into_any_element()
            } else {
                h_flex().child(control).into_any_element()
            }
        });
        v_flex()
            .w_full()
            .gap_3()
            .child(
                v_flex().w_full().child(div().text_sm().child(self.label)).when_some(self.description, |this, text| {
                    this.child(div().text_sm().text_color(cx.theme().muted_foreground).child(text))
                }),
            )
            .children(control)
    }
}
