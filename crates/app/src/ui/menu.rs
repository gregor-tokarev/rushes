//! Menus that grow with the zoom.
//!
//! The kit draws a menu item's label in a row 26 pixels high, which a label
//! zoomed past about 160% outgrows: the rows overlap. A menu item the kit
//! draws from an element is as tall as the element, so [`menu_item`] draws the
//! label as one. Grown rows can make a long menu taller than the window, so
//! such a menu goes through [`scrolling_menu`].

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    ElementId, InteractiveElement as _, ParentElement as _, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div,
};

/// How much of the window's height a menu may take before it scrolls.
const MENU_MAX_HEIGHT: f32 = 0.8;

/// The kit's `PopupMenuItem::new(label)`, as tall as its label at any zoom.
pub fn menu_item(label: impl Into<SharedString>) -> PopupMenuItem {
    let label = label.into();
    PopupMenuItem::element(move |_, _| {
        // The kit names only the items it draws itself; the item takes its
        // name from a label's value.
        div()
            .id(ElementId::Name(label.clone()))
            .role(Role::Label)
            .aria_value(label.clone())
            .flex_1()
            .child(label.clone())
    })
}

/// `menu`, scrolling once it is taller than most of the window: a menu that
/// fits at 100% may not when zoomed in.
pub fn scrolling_menu(menu: PopupMenu, window: &Window) -> PopupMenu {
    menu.max_h(window.viewport_size().height * MENU_MAX_HEIGHT).scrollable(true)
}
