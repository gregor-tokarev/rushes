//! The command palette: jump to a video, create one, go to a page, or run an
//! app action. Videos are matched fuzzily against the query; the palette
//! filters its own entries so the kit's command list only renders them.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme as _, Icon, IndexPath, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    Action, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, WeakEntity, Window, div,
};
use videocrm_core::{VideoId, ViewKind};

use super::sidebar::{display_title, video_emoji};
use super::{NewVideo, OpenSettings, ToggleSidebar, ToggleTheme, Workspace};
use crate::route::{Route, VideoTab};
use crate::state::AppState;
use crate::wizard;

/// Most videos listed at once; the query narrows the rest.
const MAX_VIDEOS: usize = 50;

/// What a palette entry does when confirmed.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteCommand {
    OpenVideo(VideoId),
    Navigate(Route),
    /// An app action; the palette handles it like its key binding.
    Action(PaletteAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    NewVideo,
    OpenSettings,
    ToggleTheme,
    ToggleSidebar,
}

impl PaletteAction {
    fn action(self) -> Box<dyn Action> {
        match self {
            PaletteAction::NewVideo => Box::new(NewVideo),
            PaletteAction::OpenSettings => Box::new(OpenSettings),
            PaletteAction::ToggleTheme => Box::new(ToggleTheme),
            PaletteAction::ToggleSidebar => Box::new(ToggleSidebar),
        }
    }
}

enum EntryIcon {
    Icon(IconName),
    Emoji(SharedString),
}

struct Entry {
    label: SharedString,
    detail: Option<SharedString>,
    icon: EntryIcon,
    command: PaletteCommand,
}

struct Group {
    label: &'static str,
    entries: Vec<Entry>,
}

/// Workspace facts the entries depend on, read when the palette renders.
#[derive(Clone, Copy)]
struct Snapshot {
    sidebar_collapsed: bool,
    dark: bool,
}

/// Owns the palette's search state and query. Opened by [`Workspace`] in a dialog.
pub struct CommandPalette {
    command: Entity<CommandState>,
    query: String,
}

impl CommandPalette {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self { command: cx.new(|cx| CommandState::new(window, cx)), query: String::new() }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Opens the palette with an empty query. Does nothing while another dialog is open.
    pub fn open(&mut self, workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        self.query.clear();
        self.command.update(cx, |command, cx| command.set_query("", window, cx));
        let palette = cx.weak_entity();
        let command = self.command.clone();
        let focus_on_mount = Rc::new(Cell::new(true));
        let width = window.rem_size() * 36.;
        window.open_dialog(cx, move |dialog, _, _| {
            let palette = palette.clone();
            let command = command.clone();
            let workspace = workspace.clone();
            let focus_on_mount = focus_on_mount.clone();
            dialog.close_button(false).p_0().w(width).content(move |content, window, cx| {
                if focus_on_mount.replace(false) {
                    let command = command.clone();
                    window.defer(cx, move |window, cx| command.update(cx, |command, cx| command.focus(window, cx)));
                }
                let Some(snapshot) = workspace.upgrade().map(|workspace| {
                    let workspace = workspace.read(cx);
                    Snapshot { sidebar_collapsed: workspace.is_sidebar_collapsed(), dark: cx.theme().is_dark() }
                }) else {
                    return content;
                };
                let groups = palette.upgrade().map(|palette| palette.read(cx).groups(snapshot, cx)).unwrap_or_default();
                content.child(render_palette(&command, groups, palette.clone(), workspace.clone(), snapshot))
            })
        });
    }

    /// The entries for the current query, in display order.
    fn groups(&self, snapshot: Snapshot, cx: &App) -> Vec<Group> {
        let query = self.query.trim();
        let state = AppState::global(cx);
        let state = state.read(cx);

        let mut videos: Vec<(i32, Entry)> = Vec::new();
        let recent: Vec<VideoId> = state.recent_videos().iter().map(|video| video.id).collect();
        let mut rows: Vec<_> = state.rows().iter().map(|row| &row.video).collect();
        // Recent first, then the most recently edited.
        rows.sort_by_key(|video| {
            (recent.iter().position(|id| *id == video.id).unwrap_or(usize::MAX), std::cmp::Reverse(video.updated_at))
        });
        for video in rows {
            let title = display_title(&video.title);
            let Some(score) = fuzzy_score(query, &title) else { continue };
            videos.push((
                score,
                Entry {
                    label: title.into(),
                    detail: Some(video.status.label().into()),
                    icon: EntryIcon::Emoji(video_emoji(video.icon.as_deref()).to_string().into()),
                    command: PaletteCommand::OpenVideo(video.id),
                },
            ));
        }
        // Stable: equal scores keep the recent-first order.
        videos.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        videos.truncate(MAX_VIDEOS);

        let create = vec![Entry {
            label: "New video…".into(),
            detail: None,
            icon: EntryIcon::Icon(IconName::Plus),
            command: PaletteCommand::Action(PaletteAction::NewVideo),
        }];

        let mut views = state.views().to_vec();
        views.sort_by_key(|view| view.position);
        let mut go_to: Vec<Entry> = views
            .into_iter()
            .map(|view| Entry {
                label: view.name.into(),
                detail: Some("View".into()),
                icon: EntryIcon::Icon(match view.kind {
                    ViewKind::Table => IconName::Sheet,
                    ViewKind::Board => IconName::SquareKanban,
                }),
                command: PaletteCommand::Navigate(Route::Database { view: Some(view.id) }),
            })
            .collect();
        go_to.extend([
            Entry {
                label: "Pipelines".into(),
                detail: None,
                icon: EntryIcon::Icon(IconName::WandSparkles),
                command: PaletteCommand::Navigate(Route::Pipelines),
            },
            Entry {
                label: "Uploads".into(),
                detail: None,
                icon: EntryIcon::Icon(IconName::CloudUpload),
                command: PaletteCommand::Navigate(Route::Uploads),
            },
            Entry {
                label: "Settings".into(),
                detail: None,
                icon: EntryIcon::Icon(IconName::Settings),
                command: PaletteCommand::Action(PaletteAction::OpenSettings),
            },
        ]);

        let actions = vec![
            Entry {
                label: if snapshot.dark { "Use light theme" } else { "Use dark theme" }.into(),
                detail: None,
                icon: EntryIcon::Icon(if snapshot.dark { IconName::Sun } else { IconName::Moon }),
                command: PaletteCommand::Action(PaletteAction::ToggleTheme),
            },
            Entry {
                label: if snapshot.sidebar_collapsed { "Show sidebar" } else { "Hide sidebar" }.into(),
                detail: None,
                icon: EntryIcon::Icon(IconName::PanelLeft),
                command: PaletteCommand::Action(PaletteAction::ToggleSidebar),
            },
        ];

        let filter = |entries: Vec<Entry>| -> Vec<Entry> {
            let mut scored: Vec<(i32, Entry)> = entries
                .into_iter()
                .filter_map(|entry| fuzzy_score(query, &entry.label).map(|score| (score, entry)))
                .collect();
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
            scored.into_iter().map(|(_, entry)| entry).collect()
        };

        [
            Group { label: "Videos", entries: videos.into_iter().map(|(_, entry)| entry).collect() },
            Group { label: "Create", entries: filter(create) },
            Group { label: "Go to", entries: filter(go_to) },
            Group { label: "Actions", entries: filter(actions) },
        ]
        .into_iter()
        .filter(|group| !group.entries.is_empty())
        .collect()
    }

    fn command_at(&self, index: IndexPath, snapshot: Snapshot, cx: &App) -> Option<PaletteCommand> {
        let groups = self.groups(snapshot, cx);
        let entry = groups.get(index.section)?.entries.get(index.row)?;
        Some(entry.command.clone())
    }
}

fn render_palette(
    command: &Entity<CommandState>,
    groups: Vec<Group>,
    palette: WeakEntity<CommandPalette>,
    workspace: WeakEntity<Workspace>,
    snapshot: Snapshot,
) -> impl IntoElement {
    let query_palette = palette.clone();
    let confirm_workspace = workspace.clone();
    let command_list = groups.into_iter().fold(
        Command::new(command)
            .bordered(false)
            .filterable(false)
            .placeholder("Search videos and commands…")
            .max_h(gpui_kit::rems(26.))
            .empty(|_, _, cx| {
                v_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .py_6()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::SearchX).size_6())
                    .child("No videos or commands match.")
            })
            .on_query(move |query, _, cx| {
                _ = query_palette.update(cx, |palette, cx| {
                    palette.query = query.to_string();
                    cx.notify();
                });
            })
            .on_confirm(move |index, window, cx| {
                let Some(command) =
                    palette.upgrade().and_then(|palette| palette.read(cx).command_at(index, snapshot, cx))
                else {
                    return;
                };
                // Actions already ran through their handlers below.
                if matches!(command, PaletteCommand::Action(_)) {
                    return;
                }
                window.close_dialog(cx);
                run(command, &confirm_workspace, window, cx);
            }),
        |list, group| list.group(command_group(group)),
    );

    // The palette's own handlers for the actions its entries dispatch: the
    // dialog sits outside the workspace's element tree.
    let on = |action: PaletteAction| {
        let workspace = workspace.clone();
        move |window: &mut Window, cx: &mut App| {
            window.close_dialog(cx);
            run(PaletteCommand::Action(action), &workspace, window, cx);
        }
    };
    let (new_video, settings, theme, sidebar) = (
        on(PaletteAction::NewVideo),
        on(PaletteAction::OpenSettings),
        on(PaletteAction::ToggleTheme),
        on(PaletteAction::ToggleSidebar),
    );
    div()
        .on_action(move |_: &NewVideo, window, cx| new_video(window, cx))
        .on_action(move |_: &OpenSettings, window, cx| settings(window, cx))
        .on_action(move |_: &ToggleTheme, window, cx| theme(window, cx))
        .on_action(move |_: &ToggleSidebar, window, cx| sidebar(window, cx))
        .child(command_list)
}

fn command_group(group: Group) -> CommandGroup {
    CommandGroup::new().label(group.label).items(group.entries.into_iter().map(|entry| {
        let item = CommandItem::new().label(entry.label.clone());
        let item = match &entry.command {
            PaletteCommand::Action(action) => item.action(action.action()),
            _ => item,
        };
        match entry.icon {
            EntryIcon::Icon(icon) => item.icon(icon),
            EntryIcon::Emoji(emoji) => {
                let (label, detail) = (entry.label, entry.detail);
                item.child(move |_, cx| {
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .child(div().flex_none().size_4().flex().items_center().justify_center().text_xs().child(emoji.clone()))
                        .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                        .children(
                            detail.clone().map(|detail| div().text_xs().text_color(cx.theme().muted_foreground).child(detail)),
                        )
                })
            }
        }
    }))
}

fn run(command: PaletteCommand, workspace: &WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let Some(workspace) = workspace.upgrade() else { return };
    let state = workspace.read(cx).state().clone();
    match command {
        PaletteCommand::OpenVideo(id) => state.update(cx, |state, cx| state.open_video(id, VideoTab::Script, cx)),
        PaletteCommand::Navigate(route) => state.update(cx, |state, cx| state.navigate(route, cx)),
        PaletteCommand::Action(PaletteAction::NewVideo) => {
            // Let the palette's dialog close before the wizard's opens.
            window.defer(cx, |window, cx| wizard::open_new_video_dialog(None, window, cx));
        }
        PaletteCommand::Action(PaletteAction::OpenSettings) => {
            state.update(cx, |state, cx| state.navigate(Route::Settings, cx));
        }
        PaletteCommand::Action(PaletteAction::ToggleTheme) => {
            workspace.update(cx, |workspace, cx| workspace.toggle_theme(cx));
        }
        PaletteCommand::Action(PaletteAction::ToggleSidebar) => {
            workspace.update(cx, |workspace, cx| workspace.toggle_sidebar(cx));
        }
    }
}

/// Scores `text` against `query` as a case-insensitive subsequence: matches
/// at word starts and runs of consecutive characters score higher, gaps
/// lower. `None` when the query does not match; an empty query matches all.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if query.is_empty() {
        return Some(0);
    }
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0;
    let mut next = 0;
    let mut previous: Option<usize> = None;
    for wanted in query {
        let position = (next..text.len()).find(|&ix| text[ix] == wanted)?;
        let word_start = position == 0 || !text[position - 1].is_alphanumeric();
        score += 1;
        if word_start {
            score += 8;
        }
        match previous {
            Some(previous) if previous + 1 == position => score += 5,
            Some(previous) => score -= (position - previous - 1).min(10) as i32,
            None => score -= position.min(10) as i32,
        }
        previous = Some(position);
        next = position + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_matches_subsequences_and_prefers_word_starts() {
        assert!(fuzzy_score("dsk", "Desk setup 2026").is_some());
        assert_eq!(fuzzy_score("xyz", "Desk setup"), None);
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        let word_start = fuzzy_score("ds", "Desk setup").unwrap();
        let mid_word = fuzzy_score("es", "Desk setup").unwrap();
        assert!(word_start > mid_word);
        let consecutive = fuzzy_score("desk", "Desk setup").unwrap();
        let scattered = fuzzy_score("desk", "Do every small kit").unwrap();
        assert!(consecutive > scattered);
    }
}
