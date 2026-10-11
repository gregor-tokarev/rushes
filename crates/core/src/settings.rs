//! Typed application settings and data locations.
//!
//! `AppSettings` is persisted as one JSON value in the store's settings table
//! (`Store::load_settings` / `Store::save_settings`). Every struct uses
//! `#[serde(default)]` so settings written by an older version keep loading
//! after fields are added.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use chrono::Weekday;
use serde::{Deserialize, Serialize};

use crate::media;

/// Environment variable that relocates the database and credentials (tests, screenshots).
pub const DATA_DIR_ENV: &str = "VIDEOCRM_DATA_DIR";
/// File name of the SQLite database inside [`data_dir`].
pub const DATABASE_FILE: &str = "videocrm.db";

pub const DEFAULT_EMULATOR_GOOGLE_URL: &str = "http://127.0.0.1:4002";
pub const DEFAULT_EMULATOR_YOUTUBE_URL: &str = "http://127.0.0.1:4020";
pub const DEFAULT_AGENT_COMMAND: &str = "claude";

/// Directory holding the database and credentials: `$VIDEOCRM_DATA_DIR` when set,
/// otherwise `<platform data dir>/videocrm`.
pub fn data_dir() -> PathBuf {
    data_dir_from(std::env::var_os(DATA_DIR_ENV))
}

/// Path of the SQLite database: `<data_dir>/videocrm.db`.
pub fn database_path() -> PathBuf {
    data_dir().join(DATABASE_FILE)
}

fn data_dir_from(env_value: Option<OsString>) -> PathBuf {
    match env_value {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("videocrm"),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppSettings {
    /// Where per-video media folders are created. `None` means [`media::default_media_root`].
    pub media_root: Option<PathBuf>,
    pub youtube: YoutubeBackend,
    pub pipeline: PipelineSettings,
    pub onboarding_completed: bool,
    pub theme: ThemeMode,
    pub week_start: WeekStart,
    pub updates: UpdateSettings,
}

impl AppSettings {
    /// The configured media root, or the platform default.
    pub fn resolved_media_root(&self) -> PathBuf {
        self.media_root.clone().unwrap_or_else(media::default_media_root)
    }
}

/// Which OAuth / YouTube Data API endpoints the app talks to.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum YoutubeBackend {
    /// Real Google with the user's own OAuth client (Desktop app type).
    Google { client_id: String, client_secret: String },
    /// Local emulators (`emulators/`), for development and demos.
    Emulator { google_url: String, youtube_url: String },
}

/// Redacts the client secret so settings can be logged.
impl fmt::Debug for YoutubeBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            YoutubeBackend::Google { client_id, client_secret: _ } => {
                f.debug_struct("Google").field("client_id", client_id).field("client_secret", &"<redacted>").finish()
            }
            YoutubeBackend::Emulator { google_url, youtube_url } => {
                f.debug_struct("Emulator").field("google_url", google_url).field("youtube_url", youtube_url).finish()
            }
        }
    }
}

/// The creator's own YouTube account, once they add their OAuth client. The emulators
/// are a developer option (`VIDEOCRM_GOOGLE_URL` / `VIDEOCRM_YOUTUBE_URL` override this).
impl Default for YoutubeBackend {
    fn default() -> Self {
        YoutubeBackend::Google { client_id: String::new(), client_secret: String::new() }
    }
}

/// How AI edit pipelines launch the coding agent that runs video-use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineSettings {
    /// Executable of the headless coding agent.
    pub agent_command: String,
    /// video-use skill directory. `None` lets the pipeline look in its default locations.
    pub skill_dir: Option<PathBuf>,
    /// Model passed to the agent. `None` uses the agent's default.
    pub model: Option<String>,
    /// Appended to every edit brief (house style, channel branding, ...).
    pub extra_instructions: String,
    /// Skip the strategy approval step and render straight away.
    pub auto_approve: bool,
    pub default_preferences: EditPreferences,
}

impl Default for PipelineSettings {
    fn default() -> Self {
        Self {
            agent_command: DEFAULT_AGENT_COMMAND.to_string(),
            skill_dir: None,
            model: None,
            extra_instructions: String::new(),
            auto_approve: false,
            default_preferences: EditPreferences::default(),
        }
    }
}

/// Starting values of the edit brief form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditPreferences {
    /// Cut "um", "uh", false starts and dead air.
    pub remove_fillers: bool,
    /// Burn subtitles into the render.
    pub subtitles: bool,
    /// video-use grade preset (`none`, `subtle`, `neutral_punch`, `warm_cinematic`) or a free description.
    pub color_grade: String,
    /// Free-form target such as "8-10 minutes" or "under 60s". Empty keeps the natural length.
    pub target_length: String,
}

impl Default for EditPreferences {
    fn default() -> Self {
        Self { remove_fillers: true, subtitles: false, color_grade: "none".to_string(), target_length: String::new() }
    }
}

/// How VideoCRM keeps itself up to date.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Check for new releases every few hours and install them in the
    /// background; they take effect the next time VideoCRM starts.
    pub automatic: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { automatic: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    Dark,
    /// Follow the operating system appearance.
    #[default]
    System,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System];

    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
            ThemeMode::System => "System",
        }
    }
}

/// The day every calendar starts its weeks on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WeekStart {
    #[default]
    Sunday,
    Monday,
}

impl WeekStart {
    pub const ALL: [WeekStart; 2] = [WeekStart::Sunday, WeekStart::Monday];

    pub fn label(self) -> &'static str {
        match self {
            WeekStart::Sunday => "Sunday",
            WeekStart::Monday => "Monday",
        }
    }

    pub fn weekday(self) -> Weekday {
        match self {
            WeekStart::Sunday => Weekday::Sun,
            WeekStart::Monday => Weekday::Mon,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_prefers_env_override() {
        assert_eq!(data_dir_from(Some("/tmp/vcrm".into())), PathBuf::from("/tmp/vcrm"));
    }

    #[test]
    fn data_dir_ignores_empty_env_value() {
        let dir = data_dir_from(Some(OsString::new()));
        assert!(dir.ends_with("videocrm"), "{dir:?}");
        assert_eq!(dir, data_dir_from(None));
    }

    #[test]
    fn defaults_point_at_google_and_claude() {
        let settings = AppSettings::default();
        assert_eq!(settings.youtube, YoutubeBackend::Google { client_id: String::new(), client_secret: String::new() });
        assert_eq!(settings.pipeline.agent_command, "claude");
        assert!(settings.pipeline.default_preferences.remove_fillers);
        assert_eq!(settings.theme, ThemeMode::System);
        assert_eq!(settings.week_start, WeekStart::Sunday);
        assert!(!settings.onboarding_completed);
        assert!(settings.updates.automatic);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"onboarding_completed": true, "pipeline": {"auto_approve": true}}"#).unwrap();
        assert!(settings.onboarding_completed);
        assert!(settings.pipeline.auto_approve);
        assert_eq!(settings.pipeline.agent_command, "claude");
        assert_eq!(settings.youtube, YoutubeBackend::default());
        assert!(settings.updates.automatic, "settings saved before updates existed update automatically");
        assert_eq!(settings.week_start, WeekStart::Sunday, "weeks kept starting on Sunday");
    }

    #[test]
    fn week_start_round_trips() {
        let settings = AppSettings { week_start: WeekStart::Monday, ..AppSettings::default() };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""week_start":"monday""#), "{json}");
        assert_eq!(serde_json::from_str::<AppSettings>(&json).unwrap(), settings);
        assert_eq!(WeekStart::Monday.weekday(), Weekday::Mon);
        assert_eq!(WeekStart::Sunday.weekday(), Weekday::Sun);
    }

    #[test]
    fn google_backend_round_trips() {
        let settings = AppSettings {
            youtube: YoutubeBackend::Google { client_id: "id".into(), client_secret: "secret".into() },
            theme: ThemeMode::Dark,
            media_root: Some("/media".into()),
            ..AppSettings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""type":"google""#), "{json}");
        assert_eq!(serde_json::from_str::<AppSettings>(&json).unwrap(), settings);
        assert_eq!(settings.resolved_media_root(), PathBuf::from("/media"));
    }

    #[test]
    fn debug_output_redacts_the_client_secret() {
        let settings = AppSettings {
            youtube: YoutubeBackend::Google { client_id: "id-123".into(), client_secret: "GOCSPX-hunter2".into() },
            ..AppSettings::default()
        };
        let debug = format!("{settings:?}");
        assert!(debug.contains("id-123") && debug.contains("<redacted>"), "{debug}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }
}
