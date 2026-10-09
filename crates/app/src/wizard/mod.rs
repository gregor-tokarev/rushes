//! "New video" dialog: title, hook, format, template, publish date. Owned by the shell workstream.

use gpui_kit::*;
use videocrm_core::VideoStatus;

/// Opens the new-video dialog. `status` preselects the stage (board column "+").
pub fn open_new_video_dialog(_status: Option<VideoStatus>, _window: &mut Window, _cx: &mut App) {}
