//! The command palette: jump to a video, create one, go to a page, or run an
//! app action. Entries are matched fuzzily against the query and ranked by
//! how well they match, across groups: a command named in full comes before a
//! video whose title only holds the letters. The palette filters its own
//! entries so the kit's command list only renders them.

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

use super::sidebar::display_title;
use super::{NewVideo, OpenSettings, ToggleSidebar, ToggleTheme, Workspace};
use crate::route::{Route, SettingsSection, VideoTab};
use crate::state::AppState;
use crate::ui::PageIcon;
use crate::wizard;

/// Most videos listed at once; the query narrows the rest.
const MAX_VIDEOS: usize = 50;

/// What a palette entry does when confirmed.
#[derive(Clone, Debug, PartialEq)]
enum PaletteCommand {
    OpenVideo(VideoId),
    Navigate(Route),
    /// An app action; the palette handles it like its key binding.
    Action(PaletteAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteAction {
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
    /// A video's page icon; `None` when it has none.
    Page(Option<SharedString>),
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

/// How well an entry matches the query; higher is better. The tier counts
/// first, the fuzzy score breaks ties within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    tier: MatchTier,
    score: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MatchTier {
    /// The query's letters in order, spread over the text.
    Scattered,
    /// The query inside a word.
    Inside,
    /// The query at the start of a word.
    WordStart,
    /// The text starts with the query.
    Prefix,
    /// The text is the query.
    Exact,
}

/// Ranks `text` for `query`; `None` when it doesn't match. An empty query matches all equally.
fn rank(query: &str, text: &str) -> Option<Rank> {
    let score = fuzzy_score(query, text)?;
    let query = query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let tier = if query.is_empty() {
        MatchTier::Scattered
    } else if text == query {
        MatchTier::Exact
    } else if text.starts_with(&query) {
        MatchTier::Prefix
    } else if text
        .match_indices(&query)
        .any(|(ix, _)| !text[..ix].chars().next_back().is_some_and(char::is_alphanumeric))
    {
        MatchTier::WordStart
    } else if text.contains(&query) {
        MatchTier::Inside
    } else {
        MatchTier::Scattered
    };
    Some(Rank { tier, score })
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

        let mut videos: Vec<(Rank, Entry)> = Vec::new();
        let recent: Vec<VideoId> = state.recent_videos().iter().map(|video| video.id).collect();
        let mut rows: Vec<_> = state.rows().iter().map(|row| &row.video).collect();
        // Recent first, then the most recently edited.
        rows.sort_by_key(|video| {
            (recent.iter().position(|id| *id == video.id).unwrap_or(usize::MAX), std::cmp::Reverse(video.updated_at))
        });
        for video in rows {
            let title = display_title(&video.title);
            let Some(rank) = rank(query, &title) else { continue };
            videos.push((
                rank,
                Entry {
                    label: title.into(),
                    detail: Some(video.status.label().into()),
                    icon: EntryIcon::Page(video.icon.clone().map(Into::into)),
                    command: PaletteCommand::OpenVideo(video.id),
                },
            ));
        }
        // Stable: equal ranks keep the recent-first order.
        videos.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
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

        let filter = |entries: Vec<Entry>| -> Vec<(Rank, Entry)> {
            let mut ranked: Vec<(Rank, Entry)> =
                entries.into_iter().filter_map(|entry| rank(query, &entry.label).map(|rank| (rank, entry))).collect();
            ranked.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
            ranked
        };

        let mut groups: Vec<(Rank, Group)> =
            [("Videos", videos), ("Create", filter(create)), ("Go to", filter(go_to)), ("Actions", filter(actions))]
                .into_iter()
                .filter_map(|(label, ranked)| {
                    let best = ranked.first()?.0;
                    Some((best, Group { label, entries: ranked.into_iter().map(|(_, entry)| entry).collect() }))
                })
                .collect();
        // The best match leads, so Enter runs it. Stable: an empty query keeps the group order.
        groups.sort_by_key(|(best, _)| std::cmp::Reverse(*best));
        groups.into_iter().map(|(_, group)| group).collect()
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
            EntryIcon::Page(icon) => {
                let (label, detail) = (entry.label, entry.detail);
                item.child(move |_, cx| {
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .child(PageIcon::new(icon.as_deref()))
                        .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                        .children(
                            detail
                                .clone()
                                .map(|detail| div().text_xs().text_color(cx.theme().muted_foreground).child(detail)),
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
            state.update(cx, |state, cx| state.open_settings(SettingsSection::General, cx));
        }
        PaletteCommand::Action(PaletteAction::ToggleTheme) => {
            workspace.update(cx, |workspace, cx| workspace.toggle_theme(cx));
        }
        PaletteCommand::Action(PaletteAction::ToggleSidebar) => {
            workspace.update(cx, |workspace, cx| workspace.toggle_sidebar(cx));
        }
    }
}

/// Bonus for a character matched at the start of a word.
const WORD_START: i32 = 8;
/// Bonus for a character matched right after the previous one.
const CONSECUTIVE: i32 = 5;
/// Largest penalty for the characters skipped before a match.
const MAX_GAP: usize = 10;

/// A placement of the query in the text: its score and where it starts.
type Placement = (i32, usize);

/// Scores `text` against `query` as a case-insensitive subsequence: matches
/// at word starts and runs of consecutive characters score higher, gaps
/// lower, and the best placement of the query counts. A placement that loses
/// more to gaps than it gains is the query scattered over unrelated words and
/// gives `None`. Placements further into the text rank lower. An empty query
/// matches all.
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    let Some((first, rest)) = query.split_first() else { return Some(0) };
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let char_score = |ix: usize| {
        let word_start = ix == 0 || !text[ix - 1].is_alphanumeric();
        1 + if word_start { WORD_START } else { 0 }
    };
    let gap = |skipped: usize| skipped.min(MAX_GAP) as i32;
    let better = |a: Option<Placement>, b: Option<Placement>| match (a, b) {
        (Some(a), Some(b)) => Some(if b.0 > a.0 { b } else { a }),
        (a, b) => a.or(b),
    };

    // best[ix]: the best placement of the query so far with its last character at `ix`.
    let mut best: Vec<Option<Placement>> =
        (0..text.len()).map(|ix| (text[ix] == *first).then(|| (char_score(ix), ix))).collect();
    for wanted in rest {
        let mut next = vec![None; text.len()];
        // The best placement ending at least MAX_GAP + 1 characters back, where the gap penalty is flat.
        let mut far: Option<Placement> = None;
        for ix in 0..text.len() {
            if ix > MAX_GAP {
                far = better(far, best[ix - MAX_GAP - 1]);
            }
            if text[ix] != *wanted {
                continue;
            }
            let mut previous = far.map(|(score, start)| (score - gap(MAX_GAP), start));
            let near = ix.saturating_sub(MAX_GAP);
            for (offset, placement) in best[near..ix].iter().enumerate() {
                let skipped = ix - near - offset - 1;
                let step = if skipped == 0 { CONSECUTIVE } else { -gap(skipped) };
                previous = better(previous, placement.map(|(score, start)| (score + step, start)));
            }
            next[ix] = previous.map(|(score, start)| (score + char_score(ix), start));
        }
        best = next;
    }
    let (score, start) = best.into_iter().flatten().reduce(|a, b| if b.0 > a.0 { b } else { a })?;
    (score >= 0).then(|| score - gap(start))
}

#[cfg(test)]
mod tests {
    use super::{MatchTier, fuzzy_score, rank};

    #[test]
    fn a_name_typed_in_full_outranks_scattered_letters() {
        let video = "My 2026 Desk Setup: Everything That Survived";
        assert_eq!(rank("settings", video).map(|rank| rank.tier), Some(MatchTier::Scattered));
        assert!(rank("settings", "Settings") > rank("settings", video));
        assert_eq!(rank("set", "Settings").map(|rank| rank.tier), Some(MatchTier::Prefix));
        assert_eq!(rank("set", video).map(|rank| rank.tier), Some(MatchTier::WordStart));
        assert!(rank("set", "Settings") > rank("set", video));
        assert!(rank("desk setup", video) > rank("desk setup", "Settings"));
        assert_eq!(rank("show  sidebar", "Show sidebar").map(|rank| rank.tier), Some(MatchTier::Exact));
        assert_eq!(rank("etup", video).map(|rank| rank.tier), Some(MatchTier::Inside));
        assert_eq!(rank("", video), rank("", "Settings"));
    }

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

    #[test]
    fn fuzzy_finds_the_best_placement_and_drops_scattered_matches() {
        // A greedy left-most match takes the "t" of "Things"; the word "tour" is the placement that counts.
        assert!(fuzzy_score("tour", "Things to know about the desk tour").is_some_and(|score| score > 0));
        assert!(fuzzy_score("theme", "Use light theme").unwrap() > 0);
        // Letters picked from unrelated words are not a match.
        assert_eq!(fuzzy_score("theme", "Rust Ownership Finally Explained in 10 Minutes"), None);
        assert_eq!(fuzzy_score("set", "Stop Using print() to Debug Python"), None);
        // A word start late in the title still matches.
        assert!(fuzzy_score("p", "Stop Using print()").is_some());
        assert!(fuzzy_score("rst own", "Rust ownership explained").is_some());
    }
}
