//! Where the main area of the window is pointed.

use videocrm_core::{VideoId, ViewId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum VideoTab {
    #[default]
    Script,
    Notes,
    /// Footage and AI edit pipelines.
    Footage,
    /// YouTube metadata and upload.
    Publish,
}

impl VideoTab {
    pub const ALL: [VideoTab; 4] = [VideoTab::Script, VideoTab::Notes, VideoTab::Footage, VideoTab::Publish];

    pub fn label(self) -> &'static str {
        match self {
            VideoTab::Script => "Script",
            VideoTab::Notes => "Notes",
            VideoTab::Footage => "Footage & AI edit",
            VideoTab::Publish => "Publish",
        }
    }
}

/// A page of the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SettingsSection {
    /// Theme and folders.
    #[default]
    General,
    /// The YouTube account and API client.
    Youtube,
    /// The coding agent and video-use.
    AiEditing,
    About,
}

impl SettingsSection {
    /// In the order the settings list them.
    pub const ALL: [SettingsSection; 4] =
        [SettingsSection::General, SettingsSection::Youtube, SettingsSection::AiEditing, SettingsSection::About];

    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Youtube => "YouTube",
            SettingsSection::AiEditing => "AI editing",
            SettingsSection::About => "About",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Route {
    /// The videos database; `None` shows the first saved view.
    Database {
        view: Option<ViewId>,
    },
    Video {
        id: VideoId,
        tab: VideoTab,
    },
    Pipelines,
    Uploads,
    Settings {
        section: SettingsSection,
    },
    Onboarding,
}

impl Default for Route {
    fn default() -> Self {
        Route::Database { view: None }
    }
}
