//! Menu items that grow with the zoom.
//!
//! The kit draws a menu item's label in a row 26 pixels high, which a label
//! zoomed past about 160% outgrows: the rows overlap. A menu item the kit
//! draws from an element is as tall as the element, so [`menu_item`] draws the
//! label as one.

use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::{
    ElementId, InteractiveElement as _, ParentElement as _, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, div,
};

/// The kit's `PopupMenuItem::new(label)`, as tall as its label at any zoom.
pub fn menu_item(label: impl Into<SharedString>) -> PopupMenuItem {
    let label = label.into();
    PopupMenuItem::element(move |_, _| {
        // The kit names only the items it draws itself.
        div()
            .id(ElementId::Name(label.clone()))
            .role(Role::Label)
            .aria_label(label.clone())
            .flex_1()
            .child(label.clone())
    })
}
