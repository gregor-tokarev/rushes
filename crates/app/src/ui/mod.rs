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
//!   videos always goes through [`dialog::confirm_delete_videos`].
//! - **Page icons.** A video's icon is a [`PageIcon`]: a bundled colour image
//!   for every emoji the app offers, the text for any other emoji, and one
//!   neutral page glyph for a video without an icon.
//! - **Dates.** Timestamps read [`format_when`]: `Today, 14:02`.
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
pub mod dialog;
mod empty_state;
mod page;
mod page_icon;
mod row_button;
mod section;
pub mod settings;
mod table;
mod time;

pub use choice_card::ChoiceCard;
pub use empty_state::EmptyState;
pub use page::{PAGE_INSET, PageHeader, page_frame};
pub use page_icon::{Assets, PageIcon, has_page_icon_image, video_icon};
pub use row_button::{RowButton, section_label};
pub use section::{Card, SectionHeader};
pub use table::{TableColumn, TableRowButton, table_header};
pub use time::format_when;
