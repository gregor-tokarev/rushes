//! Notion-like videos database: table and board views.
//!
//! [`DatabasePanel`] renders the database and reports every change as a
//! [`DatabaseEvent`]; the owner persists changes and hands the stored state
//! back with [`DatabasePanel::set_data`]. Register [`WithDatabaseIcons`] around
//! the application's asset source so the panel's icons resolve.

mod board;
pub mod colors;
mod elements;
pub mod format;
mod icons;
mod panel;
mod property_editor;
pub mod query;
mod table;
mod toolbar;

pub use elements::Chip;
pub(crate) use elements::{NO_OPTIONS_ON_PAGE, multi_select_list, select_menu};
pub(crate) use icons::kind_icon;
pub use icons::{DatabaseIcons, WithDatabaseIcons};
pub use panel::{DatabaseEvent, DatabasePanel};
pub(crate) use property_editor::{PropertyEditor, PropertyEditorMode};
