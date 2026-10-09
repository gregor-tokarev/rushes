//! Sections of a page or tab, and the one card surface.

use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, SharedString,
    StyleRefinement, Styled, TestSupportExt as _, Window, div,
};

/// The heading of a region within a page or tab: a title, actions on its
/// trailing side and an optional muted description below.
#[derive(IntoElement)]
pub struct SectionHeader {
    title: SharedString,
    description: Option<SharedString>,
    actions: Vec<AnyElement>,
}

impl SectionHeader {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self { title: title.into(), description: None, actions: Vec::new() }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn actions(mut self, actions: impl IntoIterator<Item = AnyElement>) -> Self {
        self.actions.extend(actions);
        self
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .gap_0p5()
            .child(
                h_flex()
                    .gap_2()
                    .min_h_6()
                    .child(div().flex_1().min_w_0().text_base().font_semibold().child(self.title))
                    .children(self.actions),
            )
            .when_some(self.description, |this, description| {
                this.child(div().text_sm().text_color(cx.theme().muted_foreground).child(description))
            })
    }
}

/// The one outlined surface of the app's pages: a hairline border on the
/// large radius, inset by `p_4`. Its title, when it has one, sits inside it,
/// with an optional leading icon, a description and actions on the title's
/// trailing side. Style refinements (gap, border colour) apply over these
/// defaults.
#[derive(IntoElement)]
pub struct Card {
    style: StyleRefinement,
    id: Option<ElementId>,
    icon: Option<Icon>,
    title: Option<SharedString>,
    description: Option<SharedString>,
    actions: Vec<AnyElement>,
    children: Vec<AnyElement>,
}

impl Card {
    pub fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            id: None,
            icon: None,
            title: None,
            description: None,
            actions: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Makes the card observable by this id in tests and keyed for state.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// An icon before the title, for a card that reports a state.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl Default for Card {
    fn default() -> Self {
        Self::new()
    }
}

impl Styled for Card {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Card {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Card {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let has_heading = self.title.is_some() || !self.actions.is_empty();
        let heading = has_heading.then(|| {
            h_flex()
                .gap_3()
                .items_start()
                .children(self.icon.map(|icon| div().pt_0p5().child(icon.small())))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .children(self.title.map(|title| div().text_sm().font_semibold().child(title)))
                        .children(
                            self.description.map(|text| div().text_sm().text_color(theme.muted_foreground).child(text)),
                        ),
                )
                .children(self.actions)
        });
        let card = v_flex()
            .gap_3()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .refine_style(&self.style)
            .children(heading)
            .children(self.children);
        match self.id {
            Some(id) => card.id(id).test_support().into_any_element(),
            None => card.into_any_element(),
        }
    }
}
