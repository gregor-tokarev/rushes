//! The application's own presentation layer: the pieces every page shares so
//! VideoCRM reads as one product.
//!
//! # Grammar
//!
//! - **Pages.** A top-level page (Videos, Pipelines, Uploads) opens with a
//!   [`PageHeader`] inside [`page_frame`]: one leading inset ([`PAGE_INSET`]),
//!   a `text_2xl` semibold title and a muted one-line subtitle that carries the
//!   page's count or summary. Collections below it are bare tables on the
//!   kit's muted header fill, without an outer card.
//! - **Sections.** Within a page or tab, a [`SectionHeader`] (`text_base`
//!   semibold title, muted `text_sm` description, trailing actions) introduces
//!   each region. A [`Card`] is the one outlined surface; its title, when it has
//!   one, sits inside it.
//! - **Settings.** Every settings section is built from the kit's setting
//!   groups and items: a muted group title, label and description on the
//!   leading side, the control trailing or stacked below, text fields capped at
//!   [`settings::FIELD_MAX_WIDTH`]. Views whose settings also appear in
//!   onboarding describe them once as [`settings::SettingsSection`]s.
//! - **Empty states.** [`EmptyState`]: muted icon without a disc, `text_base`
//!   semibold title, muted description and, where there is one, the next action
//!   as a small outline button, centred in the region it stands for.
//! - **Dialogs.** Form dialogs end in [`dialog::form_footer`]: trailing,
//!   content-sized Cancel and commit, as the kit's alert dialogs do.
//!   Destructive confirmations name the object and the consequence; deleting
//!   videos always goes through [`dialog::confirm_delete_videos`]. Their
//!   commit is a solid danger button (the theme sets it up in
//!   `shell::apply_theme`).
//! - **Choosing paths.** When the system file or folder chooser can't open,
//!   every place falls back to [`path_prompt::open`]: one dialog that takes a
//!   typed path and says next to the field why a path can't be used.
//! - **Status colours.** The theme's status colours are fills; warning text
//!   uses [`warning_text`] and a warning count [`contrast::warning_badge`],
//!   which read in both themes.
//! - **Page icons.** A video's icon is a [`PageIcon`]: a bundled colour image
//!   for every emoji the app offers, the text for any other emoji, and one
//!   neutral page glyph for a video without an icon.
//! - **Dates.** Timestamps read [`format_when`]: `Today, 14:02`.
//! - **Focus.** Every row, card and control the app makes focusable itself
//!   takes focus through [`KeyboardFocus`]: a click doesn't take the keyboard,
//!   and the focus ring shows only focus the keyboard gave.
//! - **Tooltips.** A tooltip that explains a passing state, such as why a
//!   control is disabled, is a [`state_tooltip`], which leaves with the state.
//!
//! # Button sizes
//!
//! - In-page actions — section headers, cards, table rows, toolbars, empty
//!   states — are `small()`. A decision area's primary commit (Start AI edit,
//!   Approve & render, Upload to YouTube) is `small()` too: the same size as the
//!   actions beside it, distinguished by its variant only.
//! - `xsmall()` is for controls inside a dense row or cell (a tag's remove
//!   button, the table's hover Open), never for a section's commands.
//! - Dialog footers and onboarding keep the kit's default (medium) size.
//! - The same command keeps its label, icon and variant everywhere: "Open
//!   folder" is a ghost button with the folder icon, a YouTube link is a
//!   [`gpui_kit::component::link::Link`] ending in the external-link icon.

mod choice_card;
pub mod contrast;
pub mod dialog;
mod empty_state;
mod focus;
mod page;
mod page_icon;
pub mod path_prompt;
mod row_button;
mod section;
pub mod settings;
mod state_tooltip;
mod table;
mod time;

use gpui_kit::App;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;

pub use choice_card::ChoiceCard;
pub use contrast::warning_text;
pub use empty_state::EmptyState;
pub use focus::KeyboardFocus;
pub use page::{PAGE_INSET, PageHeader, page_frame};
pub use page_icon::{Assets, PageIcon, has_page_icon_image, video_icon};
pub use row_button::{RowButton, section_label};
pub use section::{Card, SectionHeader};
pub use state_tooltip::state_tooltip;
pub use table::{CollectionTable, TableColumn, TableRowButton, table_header};
pub use time::format_when;

/// Shows a notification in the active window, for services that have no
/// window of their own; nothing happens without a window.
pub fn push_notification(cx: &mut App, notification: impl FnOnce() -> Notification) {
    let window = cx.active_window().or_else(|| cx.windows().into_iter().next());
    if let Some(window) = window {
        let notification = notification();
        // The window may have closed since; then there is nobody to tell.
        let _ = window.update(cx, |_, window, cx| window.push_notification(notification, cx));
    }
}
