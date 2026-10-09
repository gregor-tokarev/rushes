//! Filtering, sorting and column resolution for a [`DatabaseView`].
//!
//! Pure functions over the core model, so views and tests share one source of
//! truth for "which videos does this view show, in which order, with which
//! columns".

use std::cmp::Ordering;

use chrono::{DateTime, NaiveDate, Utc};
use videocrm_core::{
    BuiltinField, Column, ColumnLayout, DatabaseView, FilterSpec, PropertyDef, PropertyKind, PropertyValue, RunStatus,
    SortDirection, SortSpec, UploadStatus, VideoFormat, VideoRow, VideoStatus, YoutubeState,
};

/// Indices of the `rows` that match `filter`, in input order.
///
/// The search is split on whitespace and every term must occur, case-insensitively,
/// in the title, the hook or one of the tags.
pub fn apply_filter(rows: &[VideoRow], filter: &FilterSpec) -> Vec<usize> {
    let terms = search_terms(&filter.search);
    rows.iter().enumerate().filter(|(_, row)| matches_filter(row, filter, &terms)).map(|(ix, _)| ix).collect()
}

/// Reorders `indices` (into `rows`) by `sorts`, applied in order.
///
/// Empty values sort last in both directions. Ties, and views without sorts,
/// fall back to the manual `position` order.
pub fn apply_sorts(rows: &[VideoRow], indices: &mut [usize], sorts: &[SortSpec], properties: &[PropertyDef]) {
    let mut keyed: Vec<(Vec<SortKey>, usize)> = indices
        .iter()
        .map(|&ix| {
            let keys = sorts.iter().map(|sort| sort_key(&rows[ix], sort.column, properties)).collect();
            (keys, ix)
        })
        .collect();

    keyed.sort_by(|(a_keys, a_ix), (b_keys, b_ix)| {
        sorts
            .iter()
            .zip(a_keys.iter().zip(b_keys))
            .map(|(sort, (a, b))| a.compare(b, sort.direction))
            .find(|ordering| ordering.is_ne())
            .unwrap_or_else(|| manual_order(&rows[*a_ix], &rows[*b_ix]))
    });

    for (slot, (_, ix)) in indices.iter_mut().zip(keyed) {
        *slot = ix;
    }
}

/// The rows `view` shows, as indices into `rows`: filtered, then sorted.
pub fn view_rows(rows: &[VideoRow], view: &DatabaseView, properties: &[PropertyDef]) -> Vec<usize> {
    let mut indices = apply_filter(rows, &view.filter);
    apply_sorts(rows, &mut indices, &view.sorts, properties);
    indices
}

/// Every column of `view` in display order, reconciled with `properties`.
///
/// Title is always first and visible. Layouts of deleted properties and
/// duplicates are dropped; built-in fields missing from the view are appended
/// visible and custom properties missing from it are appended hidden, as
/// documented on [`DatabaseView::columns`].
pub fn resolve_columns(view: &DatabaseView, properties: &[PropertyDef]) -> Vec<ColumnLayout> {
    let exists = |column: &Column| match column {
        Column::Builtin(_) => true,
        Column::Custom(id) => properties.iter().any(|property| property.id == *id),
    };

    let title = view.columns.iter().find(|layout| layout.column == Column::Builtin(BuiltinField::Title)).map_or_else(
        || column_layout(Column::Builtin(BuiltinField::Title), properties, true),
        |layout| ColumnLayout { visible: true, ..layout.clone() },
    );

    let mut columns = vec![title];
    for layout in &view.columns {
        if exists(&layout.column) && !columns.iter().any(|known| known.column == layout.column) {
            columns.push(layout.clone());
        }
    }

    for field in BuiltinField::ALL {
        let column = Column::Builtin(field);
        if !columns.iter().any(|known| known.column == column) {
            columns.push(column_layout(column, properties, true));
        }
    }

    let mut custom: Vec<&PropertyDef> = properties.iter().collect();
    custom.sort_by_key(|property| property.position);
    for property in custom {
        let column = Column::Custom(property.id);
        if !columns.iter().any(|known| known.column == column) {
            columns.push(column_layout(column, properties, false));
        }
    }

    columns
}

/// The visible subset of [`resolve_columns`], in display order.
pub fn visible_columns(view: &DatabaseView, properties: &[PropertyDef]) -> Vec<ColumnLayout> {
    resolve_columns(view, properties).into_iter().filter(|layout| layout.visible).collect()
}

/// Number of active filter rules, shown as a count on the Filter button.
/// The search query is visible in its own field and is not counted.
pub fn active_filter_count(filter: &FilterSpec) -> usize {
    usize::from(!filter.statuses.is_empty())
        + usize::from(!filter.formats.is_empty())
        + usize::from(filter.hide_archived)
}

/// Whether a video with `status` can pass `filter`'s status rules.
///
/// Boards use it to decide which status columns exist: Archived only appears
/// when the filter lets archived videos through.
pub fn status_visible(status: VideoStatus, filter: &FilterSpec) -> bool {
    if !filter.statuses.is_empty() {
        return filter.statuses.contains(&status);
    }
    !(filter.hide_archived && status == VideoStatus::Archived)
}

/// A manual `position` strictly between two neighbours, for drag and drop
/// reordering. A missing neighbour means the start or the end of the list.
pub fn position_between(previous: Option<f64>, next: Option<f64>) -> f64 {
    match (previous, next) {
        (Some(previous), Some(next)) => previous + (next - previous) / 2.0,
        (Some(previous), None) => previous + 1.0,
        (None, Some(next)) => next - 1.0,
        (None, None) => 0.0,
    }
}

/// Display name of a column. A property that no longer exists has no name.
pub fn column_name(column: Column, properties: &[PropertyDef]) -> Option<String> {
    match column {
        Column::Builtin(field) => Some(field.label().to_string()),
        Column::Custom(id) => {
            properties.iter().find(|property| property.id == id).map(|property| property.name.clone())
        }
    }
}

/// Width of a column that the view has not sized yet, in pixels.
pub fn default_width(column: Column, properties: &[PropertyDef]) -> f32 {
    match column {
        Column::Builtin(field) => match field {
            BuiltinField::Title => 300.0,
            BuiltinField::Tags => 220.0,
            BuiltinField::Script => 180.0,
            BuiltinField::Format | BuiltinField::Youtube => 130.0,
            BuiltinField::Status
            | BuiltinField::PublishDate
            | BuiltinField::Edit
            | BuiltinField::CreatedAt
            | BuiltinField::UpdatedAt => 140.0,
        },
        Column::Custom(id) => match properties.iter().find(|property| property.id == id) {
            Some(PropertyDef { kind: PropertyKind::Number, .. }) => 120.0,
            Some(PropertyDef { kind: PropertyKind::Checkbox, .. }) => 110.0,
            Some(PropertyDef {
                kind: PropertyKind::MultiSelect { .. } | PropertyKind::Url | PropertyKind::Text,
                ..
            }) => 200.0,
            _ => 150.0,
        },
    }
}

fn column_layout(column: Column, properties: &[PropertyDef], visible: bool) -> ColumnLayout {
    ColumnLayout { column, width: default_width(column, properties), visible }
}

fn search_terms(search: &str) -> Vec<String> {
    search.split_whitespace().map(str::to_lowercase).collect()
}

fn matches_filter(row: &VideoRow, filter: &FilterSpec, terms: &[String]) -> bool {
    let video = &row.video;
    if !status_visible(video.status, filter) {
        return false;
    }
    if !filter.formats.is_empty() && !filter.formats.contains(&video.format) {
        return false;
    }
    if terms.is_empty() {
        return true;
    }

    let title = video.title.to_lowercase();
    let hook = video.hook.to_lowercase();
    let tags: Vec<String> = video.tags.iter().map(|tag| tag.to_lowercase()).collect();
    terms.iter().all(|term| {
        title.contains(term.as_str())
            || hook.contains(term.as_str())
            || tags.iter().any(|tag| tag.contains(term.as_str()))
    })
}

fn manual_order(a: &VideoRow, b: &VideoRow) -> Ordering {
    a.video
        .position
        .total_cmp(&b.video.position)
        .then_with(|| a.video.created_at.cmp(&b.video.created_at))
        .then_with(|| a.video.id.cmp(&b.video.id))
}

/// A comparable projection of one cell. `Empty` sorts after every value.
#[derive(Debug, PartialEq)]
enum SortKey {
    Empty,
    Text(String),
    Number(f64),
    Date(NaiveDate),
    Timestamp(DateTime<Utc>),
    Rank(usize),
    Ranks(Vec<usize>),
    Flag(bool),
}

impl SortKey {
    fn compare(&self, other: &Self, direction: SortDirection) -> Ordering {
        let ordering = match (self, other) {
            (SortKey::Empty, SortKey::Empty) => return Ordering::Equal,
            (SortKey::Empty, _) => return Ordering::Greater,
            (_, SortKey::Empty) => return Ordering::Less,
            (SortKey::Text(a), SortKey::Text(b)) => a.cmp(b),
            (SortKey::Number(a), SortKey::Number(b)) => a.total_cmp(b),
            (SortKey::Date(a), SortKey::Date(b)) => a.cmp(b),
            (SortKey::Timestamp(a), SortKey::Timestamp(b)) => a.cmp(b),
            (SortKey::Rank(a), SortKey::Rank(b)) => a.cmp(b),
            (SortKey::Ranks(a), SortKey::Ranks(b)) => a.cmp(b),
            (SortKey::Flag(a), SortKey::Flag(b)) => a.cmp(b),
            // Keys of one column always share a variant.
            _ => Ordering::Equal,
        };
        match direction {
            SortDirection::Ascending => ordering,
            SortDirection::Descending => ordering.reverse(),
        }
    }
}

fn text_key(text: &str) -> SortKey {
    let text = text.trim();
    if text.is_empty() { SortKey::Empty } else { SortKey::Text(text.to_lowercase()) }
}

fn sort_key(row: &VideoRow, column: Column, properties: &[PropertyDef]) -> SortKey {
    let video = &row.video;
    match column {
        Column::Builtin(field) => match field {
            BuiltinField::Title => text_key(&video.title),
            BuiltinField::Status => SortKey::Rank(status_rank(video.status)),
            BuiltinField::Format => SortKey::Rank(format_rank(video.format)),
            BuiltinField::PublishDate => video.publish_date.map_or(SortKey::Empty, SortKey::Date),
            BuiltinField::Tags => text_key(&video.tags.join(", ")),
            BuiltinField::Script => SortKey::Number(row.script_words as f64),
            // The states the cells show, so sorting follows what is on screen.
            BuiltinField::Edit => row.edit_status().map_or(SortKey::Empty, |run| SortKey::Rank(run_rank(run))),
            BuiltinField::Youtube => match row.youtube_state() {
                Some(YoutubeState::Live(_)) => {
                    SortKey::Rank(upload_rank(&UploadStatus::Done { video_id: String::new() }))
                }
                Some(YoutubeState::Upload(upload)) => SortKey::Rank(upload_rank(upload)),
                None => SortKey::Empty,
            },
            BuiltinField::CreatedAt => SortKey::Timestamp(video.created_at),
            BuiltinField::UpdatedAt => SortKey::Timestamp(video.updated_at),
        },
        Column::Custom(id) => {
            let Some(property) = properties.iter().find(|property| property.id == id) else {
                return SortKey::Empty;
            };
            custom_sort_key(&property.kind, video.properties.get(&id))
        }
    }
}

fn custom_sort_key(kind: &PropertyKind, value: Option<&PropertyValue>) -> SortKey {
    let option_rank = |option_id: &str| kind.options().iter().position(|option| option.id == option_id);
    match (kind, value) {
        (PropertyKind::Checkbox, None) => SortKey::Flag(false),
        (_, None) => SortKey::Empty,
        (PropertyKind::Text, Some(PropertyValue::Text(text))) | (PropertyKind::Url, Some(PropertyValue::Url(text))) => {
            text_key(text)
        }
        (PropertyKind::Number, Some(PropertyValue::Number(number))) => SortKey::Number(*number),
        (PropertyKind::Select { .. }, Some(PropertyValue::Select(Some(option_id)))) => {
            option_rank(option_id).map_or(SortKey::Empty, SortKey::Rank)
        }
        (PropertyKind::MultiSelect { .. }, Some(PropertyValue::MultiSelect(option_ids))) => {
            let mut ranks: Vec<usize> = option_ids.iter().filter_map(|id| option_rank(id)).collect();
            ranks.sort_unstable();
            if ranks.is_empty() { SortKey::Empty } else { SortKey::Ranks(ranks) }
        }
        (PropertyKind::Date, Some(PropertyValue::Date(Some(date)))) => SortKey::Date(*date),
        (PropertyKind::Checkbox, Some(PropertyValue::Checkbox(checked))) => SortKey::Flag(*checked),
        // Unset selects and dates, or a value left over from another property kind.
        _ => SortKey::Empty,
    }
}

fn status_rank(status: VideoStatus) -> usize {
    VideoStatus::ALL.iter().position(|known| *known == status).unwrap_or(usize::MAX)
}

fn format_rank(format: VideoFormat) -> usize {
    VideoFormat::ALL.iter().position(|known| *known == format).unwrap_or(usize::MAX)
}

/// Pipeline progression order.
fn run_rank(status: RunStatus) -> usize {
    match status {
        RunStatus::Queued => 0,
        RunStatus::Planning => 1,
        RunStatus::AwaitingApproval => 2,
        RunStatus::Rendering => 3,
        RunStatus::Succeeded => 4,
        RunStatus::Failed => 5,
        RunStatus::Cancelled => 6,
    }
}

/// Upload progression order.
fn upload_rank(status: &UploadStatus) -> usize {
    match status {
        UploadStatus::Pending => 0,
        UploadStatus::Uploading { .. } => 1,
        UploadStatus::Processing { .. } => 2,
        UploadStatus::Done { .. } => 3,
        UploadStatus::Failed { .. } => 4,
        UploadStatus::Cancelled => 5,
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use videocrm_core::{PropertyId, SelectOption, Video, ViewId, ViewKind};

    use super::*;

    fn row(title: &str, position: f64) -> VideoRow {
        let mut video = Video::new(title);
        video.position = position;
        video.created_at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        video.updated_at = video.created_at;
        VideoRow {
            video,
            script_words: 0,
            script_seconds: 0,
            latest_run: None,
            has_finished_edit: false,
            latest_upload: None,
        }
    }

    fn titles(rows: &[VideoRow], indices: &[usize]) -> Vec<String> {
        indices.iter().map(|&ix| rows[ix].video.title.clone()).collect()
    }

    fn sorted(rows: &[VideoRow], sorts: &[SortSpec], properties: &[PropertyDef]) -> Vec<String> {
        let mut indices: Vec<usize> = (0..rows.len()).collect();
        apply_sorts(rows, &mut indices, sorts, properties);
        titles(rows, &indices)
    }

    fn sort(column: Column, direction: SortDirection) -> SortSpec {
        SortSpec { column, direction }
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn view(columns: Vec<ColumnLayout>) -> DatabaseView {
        DatabaseView {
            id: ViewId::new(),
            name: "All videos".into(),
            kind: ViewKind::Table,
            columns,
            sorts: Vec::new(),
            filter: FilterSpec::default(),
            position: 0,
        }
    }

    fn select_property(options: &[&str]) -> PropertyDef {
        PropertyDef {
            id: PropertyId::new(),
            name: "Priority".into(),
            kind: PropertyKind::Select {
                options: options
                    .iter()
                    .map(|name| SelectOption {
                        id: name.to_lowercase(),
                        name: (*name).into(),
                        color: Default::default(),
                    })
                    .collect(),
            },
            position: 0,
        }
    }

    #[test]
    fn search_matches_title_hook_and_tags_case_insensitively() {
        let mut rows = vec![row("Rust in 100 seconds", 0.0), row("Desk tour", 1.0), row("Vlog", 2.0)];
        rows[1].video.hook = "My MINIMAL setup".into();
        rows[2].video.tags = vec!["Travel".into(), "Japan".into()];

        let search = |query: &str| {
            let filter = FilterSpec { search: query.into(), ..FilterSpec::default() };
            titles(&rows, &apply_filter(&rows, &filter))
        };

        assert_eq!(search("RUST"), ["Rust in 100 seconds"]);
        assert_eq!(search("minimal"), ["Desk tour"]);
        assert_eq!(search("japan"), ["Vlog"]);
        assert_eq!(search("  "), ["Rust in 100 seconds", "Desk tour", "Vlog"]);
        assert!(search("python").is_empty());
    }

    #[test]
    fn every_search_term_must_match_somewhere() {
        let mut rows = vec![row("Kyoto vlog", 0.0), row("Kyoto food", 1.0)];
        rows[0].video.tags = vec!["travel".into()];

        let filter = FilterSpec { search: "kyoto TRAVEL".into(), ..FilterSpec::default() };
        assert_eq!(titles(&rows, &apply_filter(&rows, &filter)), ["Kyoto vlog"]);
    }

    #[test]
    fn status_and_format_filters_combine() {
        let mut rows = vec![row("Idea long", 0.0), row("Editing short", 1.0), row("Editing long", 2.0)];
        rows[0].video.status = VideoStatus::Idea;
        rows[1].video.status = VideoStatus::Editing;
        rows[1].video.format = VideoFormat::Short;
        rows[2].video.status = VideoStatus::Editing;

        let filter = FilterSpec { statuses: vec![VideoStatus::Editing], ..FilterSpec::default() };
        assert_eq!(titles(&rows, &apply_filter(&rows, &filter)), ["Editing short", "Editing long"]);

        let filter = FilterSpec {
            statuses: vec![VideoStatus::Editing],
            formats: vec![VideoFormat::LongForm],
            ..FilterSpec::default()
        };
        assert_eq!(titles(&rows, &apply_filter(&rows, &filter)), ["Editing long"]);
    }

    #[test]
    fn hide_archived_yields_to_an_explicit_status_filter() {
        let mut rows = vec![row("Live", 0.0), row("Old", 1.0)];
        rows[1].video.status = VideoStatus::Archived;

        let hide = FilterSpec { hide_archived: true, ..FilterSpec::default() };
        assert_eq!(titles(&rows, &apply_filter(&rows, &hide)), ["Live"]);

        let archived =
            FilterSpec { hide_archived: true, statuses: vec![VideoStatus::Archived], ..FilterSpec::default() };
        assert_eq!(titles(&rows, &apply_filter(&rows, &archived)), ["Old"]);

        assert_eq!(apply_filter(&rows, &FilterSpec::default()).len(), 2);
    }

    #[test]
    fn status_columns_follow_the_filter() {
        let mut filter = FilterSpec { hide_archived: true, ..FilterSpec::default() };
        assert!(status_visible(VideoStatus::Idea, &filter));
        assert!(!status_visible(VideoStatus::Archived, &filter));

        filter.statuses = vec![VideoStatus::Archived, VideoStatus::Ready];
        assert!(status_visible(VideoStatus::Archived, &filter));
        assert!(!status_visible(VideoStatus::Idea, &filter));
    }

    #[test]
    fn no_sorts_means_manual_position_order() {
        let rows = vec![row("Third", 3.0), row("First", -1.0), row("Second", 0.5)];
        assert_eq!(sorted(&rows, &[], &[]), ["First", "Second", "Third"]);
    }

    #[test]
    fn equal_sort_keys_fall_back_to_position() {
        let mut rows = vec![row("B", 2.0), row("A", 1.0), row("C", 0.0)];
        for row in &mut rows {
            row.video.status = VideoStatus::Editing;
        }
        rows[2].video.status = VideoStatus::Idea;

        let sorts = [sort(Column::Builtin(BuiltinField::Status), SortDirection::Ascending)];
        assert_eq!(sorted(&rows, &sorts, &[]), ["C", "A", "B"]);
    }

    #[test]
    fn title_sorts_case_insensitively_in_both_directions() {
        let rows = vec![row("banana", 0.0), row("Apple", 1.0), row("cherry", 2.0)];
        let title = Column::Builtin(BuiltinField::Title);
        assert_eq!(sorted(&rows, &[sort(title, SortDirection::Ascending)], &[]), ["Apple", "banana", "cherry"]);
        assert_eq!(sorted(&rows, &[sort(title, SortDirection::Descending)], &[]), ["cherry", "banana", "Apple"]);
    }

    #[test]
    fn status_and_format_sort_by_declaration_order() {
        let mut rows = vec![row("Published", 0.0), row("Idea", 1.0), row("Editing", 2.0)];
        rows[0].video.status = VideoStatus::Published;
        rows[1].video.status = VideoStatus::Idea;
        rows[2].video.status = VideoStatus::Editing;
        rows[0].video.format = VideoFormat::Livestream;
        rows[1].video.format = VideoFormat::Short;

        let status = Column::Builtin(BuiltinField::Status);
        assert_eq!(sorted(&rows, &[sort(status, SortDirection::Ascending)], &[]), ["Idea", "Editing", "Published"]);
        let format = Column::Builtin(BuiltinField::Format);
        assert_eq!(sorted(&rows, &[sort(format, SortDirection::Descending)], &[]), ["Published", "Idea", "Editing"]);
    }

    #[test]
    fn missing_dates_sort_last_in_both_directions() {
        let mut rows = vec![row("None", 0.0), row("March", 1.0), row("January", 2.0)];
        rows[1].video.publish_date = Some(date(2026, 3, 1));
        rows[2].video.publish_date = Some(date(2026, 1, 1));

        let column = Column::Builtin(BuiltinField::PublishDate);
        assert_eq!(sorted(&rows, &[sort(column, SortDirection::Ascending)], &[]), ["January", "March", "None"]);
        assert_eq!(sorted(&rows, &[sort(column, SortDirection::Descending)], &[]), ["March", "January", "None"]);
    }

    #[test]
    fn multi_key_sorts_apply_in_order() {
        let mut rows = vec![row("b-short", 0.0), row("a-long", 1.0), row("c-long", 2.0), row("a-short", 3.0)];
        rows[0].video.format = VideoFormat::Short;
        rows[3].video.format = VideoFormat::Short;

        let sorts = [
            sort(Column::Builtin(BuiltinField::Format), SortDirection::Descending),
            sort(Column::Builtin(BuiltinField::Title), SortDirection::Ascending),
        ];
        assert_eq!(sorted(&rows, &sorts, &[]), ["a-short", "b-short", "a-long", "c-long"]);
    }

    #[test]
    fn derived_columns_sort_by_progress_with_missing_last() {
        let mut rows = vec![row("none", 0.0), row("done", 1.0), row("planning", 2.0)];
        rows[1].latest_run = Some(RunStatus::Succeeded);
        rows[2].latest_run = Some(RunStatus::Planning);
        rows[0].latest_upload = Some(UploadStatus::Done { video_id: "x".into() });
        rows[1].latest_upload = Some(UploadStatus::Uploading { sent: 1, total: 2 });
        rows[0].script_words = 900;
        rows[1].script_words = 10;
        rows[2].script_words = 400;

        let edit = Column::Builtin(BuiltinField::Edit);
        assert_eq!(sorted(&rows, &[sort(edit, SortDirection::Descending)], &[]), ["done", "planning", "none"]);
        let youtube = Column::Builtin(BuiltinField::Youtube);
        assert_eq!(sorted(&rows, &[sort(youtube, SortDirection::Ascending)], &[]), ["done", "none", "planning"]);
        let script = Column::Builtin(BuiltinField::Script);
        assert_eq!(sorted(&rows, &[sort(script, SortDirection::Descending)], &[]), ["none", "planning", "done"]);
    }

    #[test]
    fn tags_and_timestamps_sort() {
        let mut rows = vec![row("untagged", 0.0), row("zeta", 1.0), row("alpha", 2.0)];
        rows[1].video.tags = vec!["Zeta".into()];
        rows[2].video.tags = vec!["alpha".into(), "beta".into()];
        rows[1].video.updated_at = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        rows[2].video.created_at = Utc.with_ymd_and_hms(2025, 5, 1, 0, 0, 0).unwrap();

        let tags = Column::Builtin(BuiltinField::Tags);
        assert_eq!(sorted(&rows, &[sort(tags, SortDirection::Ascending)], &[]), ["alpha", "zeta", "untagged"]);
        let updated = Column::Builtin(BuiltinField::UpdatedAt);
        assert_eq!(sorted(&rows, &[sort(updated, SortDirection::Descending)], &[])[0], "zeta");
        let created = Column::Builtin(BuiltinField::CreatedAt);
        assert_eq!(sorted(&rows, &[sort(created, SortDirection::Ascending)], &[])[0], "alpha");
    }

    #[test]
    fn custom_select_sorts_by_option_order() {
        let property = select_property(&["High", "Medium", "Low"]);
        let mut rows = vec![row("low", 0.0), row("unset", 1.0), row("high", 2.0), row("medium", 3.0)];
        let set = |row: &mut VideoRow, option: Option<&str>| {
            row.video.properties.insert(property.id, PropertyValue::Select(option.map(str::to_string)));
        };
        set(&mut rows[0], Some("low"));
        set(&mut rows[1], None);
        set(&mut rows[2], Some("high"));
        set(&mut rows[3], Some("medium"));

        let column = Column::Custom(property.id);
        let properties = [property.clone()];
        assert_eq!(
            sorted(&rows, &[sort(column, SortDirection::Ascending)], &properties),
            ["high", "medium", "low", "unset"]
        );
        assert_eq!(
            sorted(&rows, &[sort(column, SortDirection::Descending)], &properties),
            ["low", "medium", "high", "unset"]
        );
    }

    #[test]
    fn custom_scalar_kinds_sort() {
        let number =
            PropertyDef { id: PropertyId::new(), name: "Budget".into(), kind: PropertyKind::Number, position: 0 };
        let done =
            PropertyDef { id: PropertyId::new(), name: "Sponsored".into(), kind: PropertyKind::Checkbox, position: 1 };
        let shoot =
            PropertyDef { id: PropertyId::new(), name: "Shoot day".into(), kind: PropertyKind::Date, position: 2 };
        let note = PropertyDef { id: PropertyId::new(), name: "Note".into(), kind: PropertyKind::Text, position: 3 };
        let properties = [number.clone(), done.clone(), shoot.clone(), note.clone()];

        let mut rows = vec![row("a", 0.0), row("b", 1.0), row("c", 2.0)];
        rows[0].video.properties.insert(number.id, PropertyValue::Number(250.0));
        rows[1].video.properties.insert(number.id, PropertyValue::Number(-5.0));
        rows[1].video.properties.insert(done.id, PropertyValue::Checkbox(true));
        rows[2].video.properties.insert(shoot.id, PropertyValue::Date(Some(date(2026, 2, 2))));
        rows[1].video.properties.insert(shoot.id, PropertyValue::Date(None));
        rows[0].video.properties.insert(note.id, PropertyValue::Text("zebra".into()));
        rows[2].video.properties.insert(note.id, PropertyValue::Text("Ant".into()));
        rows[1].video.properties.insert(note.id, PropertyValue::Text(String::new()));

        let by = |property: &PropertyDef, direction| {
            sorted(&rows, &[sort(Column::Custom(property.id), direction)], &properties)
        };
        assert_eq!(by(&number, SortDirection::Ascending), ["b", "a", "c"]);
        assert_eq!(by(&done, SortDirection::Descending), ["b", "a", "c"]);
        assert_eq!(by(&shoot, SortDirection::Ascending), ["c", "a", "b"]);
        assert_eq!(by(&note, SortDirection::Ascending), ["c", "a", "b"]);
    }

    #[test]
    fn multi_select_sorts_by_its_best_option() {
        let mut property = select_property(&["Gear", "Travel", "Food"]);
        property.kind = PropertyKind::MultiSelect { options: property.kind.options().to_vec() };
        let properties = [property.clone()];

        let mut rows = vec![row("food", 0.0), row("none", 1.0), row("travel+gear", 2.0)];
        rows[0].video.properties.insert(property.id, PropertyValue::MultiSelect(vec!["food".into()]));
        rows[2].video.properties.insert(property.id, PropertyValue::MultiSelect(vec!["travel".into(), "gear".into()]));

        assert_eq!(
            sorted(&rows, &[sort(Column::Custom(property.id), SortDirection::Ascending)], &properties),
            ["travel+gear", "food", "none"]
        );
    }

    #[test]
    fn view_rows_filters_then_sorts() {
        let mut rows = vec![row("b", 0.0), row("archived", 1.0), row("a", 2.0)];
        rows[1].video.status = VideoStatus::Archived;
        let mut view = view(Vec::new());
        view.filter.hide_archived = true;
        view.sorts = vec![sort(Column::Builtin(BuiltinField::Title), SortDirection::Ascending)];

        assert_eq!(titles(&rows, &view_rows(&rows, &view, &[])), ["a", "b"]);
    }

    #[test]
    fn columns_are_completed_and_cleaned() {
        let kept = select_property(&["High"]);
        let mut extra = select_property(&["Low"]);
        extra.position = 5;
        let deleted = PropertyId::new();
        let properties = [kept.clone(), extra.clone()];

        let view = view(vec![
            ColumnLayout { column: Column::Builtin(BuiltinField::Status), width: 99.0, visible: true },
            ColumnLayout { column: Column::Custom(deleted), width: 120.0, visible: true },
            ColumnLayout { column: Column::Custom(kept.id), width: 160.0, visible: true },
            ColumnLayout { column: Column::Builtin(BuiltinField::Title), width: 320.0, visible: false },
            ColumnLayout { column: Column::Builtin(BuiltinField::Status), width: 10.0, visible: false },
            ColumnLayout { column: Column::Builtin(BuiltinField::Tags), width: 200.0, visible: false },
        ]);

        let columns = resolve_columns(&view, &properties);
        let order: Vec<Column> = columns.iter().map(|layout| layout.column).collect();
        assert_eq!(
            order[..4],
            [
                Column::Builtin(BuiltinField::Title),
                Column::Builtin(BuiltinField::Status),
                Column::Custom(kept.id),
                Column::Builtin(BuiltinField::Tags),
            ]
        );
        assert_eq!(columns.len(), BuiltinField::ALL.len() + 2);
        assert_eq!(columns[0].width, 320.0);
        assert!(columns[0].visible, "title is always visible");
        assert_eq!(columns[1].width, 99.0, "the first duplicate wins");
        assert!(!order.contains(&Column::Custom(deleted)));

        let extra_layout = columns.iter().find(|layout| layout.column == Column::Custom(extra.id)).unwrap();
        assert!(!extra_layout.visible, "unknown custom properties start hidden");
        let format = columns.iter().find(|layout| layout.column == Column::Builtin(BuiltinField::Format)).unwrap();
        assert!(format.visible, "unknown builtin fields start visible");

        let visible: Vec<Column> = visible_columns(&view, &properties).iter().map(|l| l.column).collect();
        assert!(!visible.contains(&Column::Builtin(BuiltinField::Tags)));
        assert!(!visible.contains(&Column::Custom(extra.id)));
        assert_eq!(visible[0], Column::Builtin(BuiltinField::Title));
    }

    #[test]
    fn positions_land_between_neighbours() {
        assert_eq!(position_between(Some(1.0), Some(2.0)), 1.5);
        assert_eq!(position_between(Some(4.0), None), 5.0);
        assert_eq!(position_between(None, Some(-2.0)), -3.0);
        assert_eq!(position_between(None, None), 0.0);
        let tight = position_between(Some(1.0), Some(1.0 + f64::EPSILON * 4.0));
        assert!(tight > 1.0 && tight < 1.0 + f64::EPSILON * 4.0);
    }

    #[test]
    fn filter_count_counts_rules() {
        let mut filter = FilterSpec::default();
        assert_eq!(active_filter_count(&filter), 0);
        filter.search = "rust".into();
        assert_eq!(active_filter_count(&filter), 0);
        filter.statuses = vec![VideoStatus::Idea, VideoStatus::Ready];
        filter.hide_archived = true;
        assert_eq!(active_filter_count(&filter), 2);
    }
}
