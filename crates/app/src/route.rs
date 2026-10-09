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
    Settings,
    Onboarding,
}

impl Default for Route {
    fn default() -> Self {
        Route::Database { view: None }
    }
}
