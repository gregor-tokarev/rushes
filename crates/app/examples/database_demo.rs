//! The videos database with realistic sample data, for trying it by hand and
//! for screenshots. Events are applied to an in-memory copy the way the app
//! shell applies them to the store.
//!
//! `cargo run -p videocrm --example database_demo [-- --board] [--dark]`

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, Utc};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, WindowExt as _, v_flex};
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use videocrm::database::{DatabaseEvent, DatabasePanel, WithDatabaseIcons};
use videocrm_core::{
    BuiltinField, Column, ColumnLayout, DatabaseView, FilterSpec, PropertyDef, PropertyId,
    PropertyKind, PropertyValue, RunStatus, SelectOption, TagColor, UploadStatus, Video,
    VideoFormat, VideoId, VideoRow, VideoStatus, ViewId, ViewKind,
};

struct Demo {
    panel: Entity<DatabasePanel>,
    rows: Vec<VideoRow>,
    properties: Vec<PropertyDef>,
    views: Vec<DatabaseView>,
    _subscription: Subscription,
}

impl Demo {
    fn new(start_on_board: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (rows, properties, views) = sample_data();
        let panel = cx.new(|cx| DatabasePanel::new(window, cx));
        panel.update(cx, |panel, cx| {
            panel.set_data(rows.clone(), properties.clone(), views.clone(), window, cx);
            if start_on_board
                && let Some(board) = views.iter().find(|view| view.kind == ViewKind::Board)
            {
                panel.set_active_view(board.id, window, cx);
            }
        });
        let subscription = cx.subscribe_in(&panel, window, Self::on_database_event);
        Self {
            panel,
            rows,
            properties,
            views,
            _subscription: subscription,
        }
    }

    /// Plays the app shell: persist the change, then hand the data back.
    fn on_database_event(
        &mut self,
        panel: &Entity<DatabasePanel>,
        event: &DatabaseEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DatabaseEvent::OpenVideo(id) => {
                if let Some(row) = self.rows.iter().find(|row| row.video.id == *id) {
                    window.push_notification(format!("Opening “{}”", row.video.title), cx);
                }
                return;
            }
            DatabaseEvent::ActiveViewChanged(_) => return,
            DatabaseEvent::CreateVideo { status } => {
                let mut video = Video::new("Untitled");
                video.status = status.unwrap_or_default();
                video.position = self.next_position();
                self.rows.push(row(video, 0, None, None));
            }
            DatabaseEvent::UpdateVideo(video) => {
                if let Some(row) = self.rows.iter_mut().find(|row| row.video.id == video.id) {
                    row.video = video.clone();
                }
            }
            DatabaseEvent::DeleteVideos(ids) => {
                self.rows.retain(|row| !ids.contains(&row.video.id))
            }
            DatabaseEvent::DuplicateVideo(id) => {
                if let Some(original) = self.rows.iter().find(|row| row.video.id == *id).cloned() {
                    let mut copy = original.video.clone();
                    copy.id = VideoId::new();
                    copy.title = format!("{} (copy)", copy.title);
                    copy.position = self.next_position();
                    copy.created_at = Utc::now();
                    copy.updated_at = copy.created_at;
                    self.rows.push(row(copy, original.script_words, None, None));
                }
            }
            DatabaseEvent::CreateProperty(def) => self.properties.push(def.clone()),
            DatabaseEvent::UpdateProperty(def) => {
                if let Some(known) = self.properties.iter_mut().find(|known| known.id == def.id) {
                    *known = def.clone();
                }
            }
            DatabaseEvent::DeleteProperty(id) => {
                self.properties.retain(|def| def.id != *id);
                for row in &mut self.rows {
                    row.video.properties.remove(id);
                }
            }
            DatabaseEvent::SaveView(view) => {
                if let Some(known) = self.views.iter_mut().find(|known| known.id == view.id) {
                    *known = view.clone();
                }
            }
            DatabaseEvent::CreateView(view) => self.views.push(view.clone()),
            DatabaseEvent::DeleteView(id) => self.views.retain(|view| view.id != *id),
        }
        let (rows, properties, views) = (
            self.rows.clone(),
            self.properties.clone(),
            self.views.clone(),
        );
        panel.update(cx, |panel, cx| {
            panel.set_data(rows, properties, views, window, cx)
        });
    }

    fn next_position(&self) -> f64 {
        self.rows
            .iter()
            .map(|row| row.video.position)
            .fold(0.0, f64::max)
            + 1.0
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .px_4()
                    .pt_6()
                    .pb_2()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .child("🎬 Videos"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Every video from idea to upload."),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let start_on_board = args.iter().any(|arg| arg == "--board");
    let dark = args.iter().any(|arg| arg == "--dark");

    gpui_kit::application()
        .with_assets(WithDatabaseIcons(gpui_kit::assets::Assets))
        .run(move |cx| {
            gpui_kit::init(cx);
            if dark {
                Theme::change(ThemeMode::Dark, None, cx);
            }
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1440.), px(900.)),
                ))),
                ..WindowOptions::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Demo::new(start_on_board, window, cx))
            })
            .expect("failed to open the demo window");
        });
}

fn row(
    video: Video,
    script_words: usize,
    latest_run: Option<RunStatus>,
    latest_upload: Option<UploadStatus>,
) -> VideoRow {
    VideoRow {
        video,
        script_words,
        script_seconds: (script_words as f64 / 150.0 * 60.0).round() as u32,
        latest_run,
        latest_upload,
    }
}

fn option(id: &str, name: &str, color: TagColor) -> SelectOption {
    SelectOption {
        id: id.into(),
        name: name.into(),
        color,
    }
}

/// A dozen videos at every stage, three custom properties and two views.
fn sample_data() -> (Vec<VideoRow>, Vec<PropertyDef>, Vec<DatabaseView>) {
    let priority = PropertyDef {
        id: PropertyId::new(),
        name: "Priority".into(),
        kind: PropertyKind::Select {
            options: vec![
                option("high", "High", TagColor::Red),
                option("medium", "Medium", TagColor::Yellow),
                option("low", "Low", TagColor::Gray),
            ],
        },
        position: 0,
    };
    let platforms = PropertyDef {
        id: PropertyId::new(),
        name: "Cross-post".into(),
        kind: PropertyKind::MultiSelect {
            options: vec![
                option("tiktok", "TikTok", TagColor::Pink),
                option("instagram", "Instagram", TagColor::Purple),
                option("x", "X", TagColor::Gray),
                option("linkedin", "LinkedIn", TagColor::Blue),
            ],
        },
        position: 1,
    };
    let thumbnail = PropertyDef {
        id: PropertyId::new(),
        name: "Thumbnail".into(),
        kind: PropertyKind::Checkbox,
        position: 2,
    };

    let today = chrono::Local::now().date_naive();
    let day = |offset: i64| Some(today + Duration::days(offset));
    let now = Utc::now();

    struct Sample {
        icon: &'static str,
        title: &'static str,
        hook: &'static str,
        status: VideoStatus,
        format: VideoFormat,
        publish: Option<NaiveDate>,
        tags: &'static [&'static str],
        words: usize,
        run: Option<RunStatus>,
        upload: Option<UploadStatus>,
        priority: Option<&'static str>,
        platforms: &'static [&'static str],
        thumbnail: bool,
        age_days: i64,
    }

    let samples = [
        Sample {
            icon: "🦀",
            title: "I built a Notion clone in Rust",
            hook: "Six weeks, one GPU-accelerated UI framework and a lot of borrow checker fights.",
            status: VideoStatus::Editing,
            format: VideoFormat::LongForm,
            publish: day(6),
            tags: &["Rust", "Coding"],
            words: 1840,
            run: Some(RunStatus::Rendering),
            upload: None,
            priority: Some("high"),
            platforms: &["x", "linkedin"],
            thumbnail: true,
            age_days: 21,
        },
        Sample {
            icon: "🖥️",
            title: "Desk setup 2026: minimal and cable-free",
            hook: "Everything on my desk, and the three things I regret buying.",
            status: VideoStatus::Filming,
            format: VideoFormat::LongForm,
            publish: day(12),
            tags: &["Setup", "Gear"],
            words: 1120,
            run: None,
            upload: None,
            priority: Some("medium"),
            platforms: &["instagram"],
            thumbnail: false,
            age_days: 18,
        },
        Sample {
            icon: "⚡",
            title: "5 VS Code extensions you are not using",
            hook: "Number three saves me an hour a week.",
            status: VideoStatus::Ready,
            format: VideoFormat::Short,
            publish: day(1),
            tags: &["Coding", "Tools"],
            words: 180,
            run: Some(RunStatus::Succeeded),
            upload: Some(UploadStatus::Uploading {
                sent: 61,
                total: 100,
            }),
            priority: Some("high"),
            platforms: &["tiktok", "instagram"],
            thumbnail: true,
            age_days: 9,
        },
        Sample {
            icon: "🐧",
            title: "Why I left macOS for Linux",
            hook: "A year later: what I miss, what I don't, and what surprised me.",
            status: VideoStatus::Scripting,
            format: VideoFormat::LongForm,
            publish: day(20),
            tags: &["Linux", "Opinion"],
            words: 650,
            run: None,
            upload: None,
            priority: Some("high"),
            platforms: &[],
            thumbnail: false,
            age_days: 12,
        },
        Sample {
            icon: "🗼",
            title: "Tokyo tech stores tour",
            hook: "Akihabara, Nakano Broadway and the tiny shop that sells only keycaps.",
            status: VideoStatus::Idea,
            format: VideoFormat::LongForm,
            publish: None,
            tags: &["Travel", "Vlog"],
            words: 0,
            run: None,
            upload: None,
            priority: Some("low"),
            platforms: &[],
            thumbnail: false,
            age_days: 30,
        },
        Sample {
            icon: "⏱️",
            title: "Rust ownership in 60 seconds",
            hook: "One owner, many borrowers, zero garbage collector.",
            status: VideoStatus::Published,
            format: VideoFormat::Short,
            publish: day(-9),
            tags: &["Rust", "Shorts"],
            words: 160,
            run: Some(RunStatus::Succeeded),
            upload: Some(UploadStatus::Done {
                video_id: "dQw4w9WgXcQ".into(),
            }),
            priority: None,
            platforms: &["tiktok", "instagram", "x"],
            thumbnail: true,
            age_days: 26,
        },
        Sample {
            icon: "🎉",
            title: "Live Q&A: 100k subscribers",
            hook: "Answering your questions and opening the silver play button live.",
            status: VideoStatus::Scheduled,
            format: VideoFormat::Livestream,
            publish: day(4),
            tags: &["Community"],
            words: 220,
            run: None,
            upload: None,
            priority: Some("medium"),
            platforms: &["x"],
            thumbnail: true,
            age_days: 7,
        },
        Sample {
            icon: "⌨️",
            title: "Keychron Q1 Max review",
            hook: "The best keyboard I have used this year, with one big catch.",
            status: VideoStatus::Editing,
            format: VideoFormat::LongForm,
            publish: day(9),
            tags: &["Review", "Gear"],
            words: 1320,
            run: Some(RunStatus::AwaitingApproval),
            upload: None,
            priority: Some("medium"),
            platforms: &["instagram"],
            thumbnail: false,
            age_days: 15,
        },
        Sample {
            icon: "🤖",
            title: "How I edit videos with AI agents",
            hook: "My cutting room is now a prompt. Here is the whole pipeline.",
            status: VideoStatus::Scripting,
            format: VideoFormat::LongForm,
            publish: day(16),
            tags: &["AI", "Workflow"],
            words: 980,
            run: None,
            upload: None,
            priority: Some("high"),
            platforms: &["linkedin", "x"],
            thumbnail: false,
            age_days: 5,
        },
        Sample {
            icon: "📈",
            title: "My 2025 channel recap",
            hook: "Views, revenue and every mistake, in numbers.",
            status: VideoStatus::Archived,
            format: VideoFormat::LongForm,
            publish: day(-280),
            tags: &["Channel"],
            words: 1500,
            run: Some(RunStatus::Succeeded),
            upload: Some(UploadStatus::Done {
                video_id: "a1b2c3d4e5f".into(),
            }),
            priority: None,
            platforms: &[],
            thumbnail: true,
            age_days: 300,
        },
        Sample {
            icon: "🗄️",
            title: "SQLite is all you need",
            hook: "Your side project does not need a database server.",
            status: VideoStatus::Idea,
            format: VideoFormat::Short,
            publish: None,
            tags: &["Databases", "Coding"],
            words: 0,
            run: None,
            upload: None,
            priority: Some("medium"),
            platforms: &[],
            thumbnail: false,
            age_days: 2,
        },
        Sample {
            icon: "🔧",
            title: "Building a home server rack",
            hook: "A 12U rack, a NAS and too many Raspberry Pis.",
            status: VideoStatus::Filming,
            format: VideoFormat::LongForm,
            publish: day(25),
            tags: &["Homelab", "Gear"],
            words: 1460,
            run: Some(RunStatus::Failed),
            upload: None,
            priority: Some("low"),
            platforms: &[],
            thumbnail: false,
            age_days: 11,
        },
    ];

    let rows = samples
        .into_iter()
        .enumerate()
        .map(|(ix, sample)| {
            let mut video = Video::new(sample.title);
            video.icon = Some(sample.icon.into());
            video.hook = sample.hook.into();
            video.status = sample.status;
            video.format = sample.format;
            video.publish_date = sample.publish;
            video.tags = sample.tags.iter().map(|tag| tag.to_string()).collect();
            video.position = ix as f64;
            video.created_at =
                now - Duration::days(sample.age_days) - Duration::hours(ix as i64 * 3);
            video.updated_at = now - Duration::hours(ix as i64 * 7 + 1);
            let mut properties = BTreeMap::new();
            if let Some(level) = sample.priority {
                properties.insert(priority.id, PropertyValue::Select(Some(level.into())));
            }
            if !sample.platforms.is_empty() {
                properties.insert(
                    platforms.id,
                    PropertyValue::MultiSelect(
                        sample.platforms.iter().map(|id| id.to_string()).collect(),
                    ),
                );
            }
            properties.insert(thumbnail.id, PropertyValue::Checkbox(sample.thumbnail));
            video.properties = properties;
            row(video, sample.words, sample.run, sample.upload)
        })
        .collect();

    let layout = |column: Column, width: f32, visible: bool| ColumnLayout {
        column,
        width,
        visible,
    };
    let builtin = |field: BuiltinField| Column::Builtin(field);
    let table = DatabaseView {
        id: ViewId::new(),
        name: "All videos".into(),
        kind: ViewKind::Table,
        columns: vec![
            layout(builtin(BuiltinField::Title), 320.0, true),
            layout(builtin(BuiltinField::Status), 140.0, true),
            layout(builtin(BuiltinField::Format), 120.0, true),
            layout(builtin(BuiltinField::PublishDate), 130.0, true),
            layout(builtin(BuiltinField::Tags), 200.0, true),
            layout(Column::Custom(priority.id), 120.0, true),
            layout(builtin(BuiltinField::Script), 180.0, true),
            layout(builtin(BuiltinField::Edit), 150.0, true),
            layout(builtin(BuiltinField::Youtube), 150.0, true),
            layout(Column::Custom(platforms.id), 190.0, true),
            layout(Column::Custom(thumbnail.id), 110.0, true),
            layout(builtin(BuiltinField::UpdatedAt), 120.0, true),
            layout(builtin(BuiltinField::CreatedAt), 120.0, false),
        ],
        sorts: Vec::new(),
        filter: FilterSpec {
            hide_archived: true,
            ..FilterSpec::default()
        },
        position: 0,
    };
    let board = DatabaseView {
        id: ViewId::new(),
        name: "Pipeline".into(),
        kind: ViewKind::Board,
        columns: Vec::new(),
        sorts: Vec::new(),
        filter: FilterSpec {
            hide_archived: true,
            ..FilterSpec::default()
        },
        position: 1,
    };

    (
        rows,
        vec![priority, platforms, thumbnail],
        vec![table, board],
    )
}
