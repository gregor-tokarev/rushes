//! Muted, Notion-like chip colors for [`TagColor`].
//!
//! This module is the application's token layer for semantic tag colors: chips,
//! status pills and colored dots across the app resolve them here, from the
//! theme's color scales, so light and dark themes stay consistent.

use gpui_kit::component::{ActiveTheme as _, ColorName};
use gpui_kit::{App, Hsla};
use videocrm_core::{RunStatus, TagColor, UploadStatus, VideoFormat};

/// Resolved colors of one [`TagColor`] for the active theme.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct TagColors {
    /// Chip fill.
    pub background: Hsla,
    /// Chip text, readable on `background`.
    pub foreground: Hsla,
    /// Stronger mid-tone for dots and small indicators.
    pub dot: Hsla,
}

/// Colors of `color` in the active theme.
pub fn tag_colors(color: TagColor, cx: &App) -> TagColors {
    let (name, saturation) = palette(color);
    let tone = |scale: usize, saturation_factor: f32| {
        let mut tone = name.scale(scale);
        tone.s *= saturation * saturation_factor;
        tone
    };
    let neutral = color == TagColor::Gray;

    if cx.theme().is_dark() {
        TagColors {
            background: tone(if neutral { 700 } else { 900 }, 0.6),
            foreground: tone(100, 0.8),
            dot: tone(400, 1.0),
        }
    } else {
        TagColors {
            background: tone(if neutral { 200 } else { 100 }, 0.75),
            foreground: tone(900, 0.7),
            dot: tone(500, 1.0),
        }
    }
}

/// The theme scale a tag color is drawn from, and how much of its saturation
/// to keep. Brown has no scale of its own: it is a desaturated orange.
fn palette(color: TagColor) -> (ColorName, f32) {
    match color {
        TagColor::Gray => (ColorName::Neutral, 1.0),
        TagColor::Brown => (ColorName::Orange, 0.4),
        TagColor::Orange => (ColorName::Orange, 1.0),
        TagColor::Yellow => (ColorName::Amber, 1.0),
        TagColor::Green => (ColorName::Green, 0.8),
        TagColor::Blue => (ColorName::Sky, 0.9),
        TagColor::Purple => (ColorName::Purple, 0.8),
        TagColor::Pink => (ColorName::Pink, 0.8),
        TagColor::Red => (ColorName::Red, 0.85),
    }
}

pub fn format_color(format: VideoFormat) -> TagColor {
    match format {
        VideoFormat::LongForm => TagColor::Gray,
        VideoFormat::Short => TagColor::Purple,
        VideoFormat::Livestream => TagColor::Red,
    }
}

pub fn run_status_color(status: RunStatus) -> TagColor {
    match status {
        RunStatus::Queued | RunStatus::Cancelled => TagColor::Gray,
        RunStatus::Planning | RunStatus::Rendering => TagColor::Blue,
        RunStatus::AwaitingApproval => TagColor::Orange,
        RunStatus::Succeeded => TagColor::Green,
        RunStatus::Failed => TagColor::Red,
    }
}

pub fn upload_status_color(status: &UploadStatus) -> TagColor {
    match status {
        UploadStatus::Pending | UploadStatus::Cancelled => TagColor::Gray,
        UploadStatus::Uploading { .. } | UploadStatus::Processing { .. } => TagColor::Blue,
        UploadStatus::Done { .. } => TagColor::Green,
        UploadStatus::Failed { .. } => TagColor::Red,
    }
}

/// A stable color for a free-text tag, so the same tag looks the same
/// everywhere without storing a color for it.
pub fn free_tag_color(tag: &str) -> TagColor {
    // FNV-1a over the lowercase bytes: stable across runs and platforms.
    let hash = tag
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    TagColor::ALL[(hash % TagColor::ALL.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_tag_colors_are_stable_and_case_insensitive() {
        assert_eq!(free_tag_color("Tutorial"), free_tag_color("tutorial"));
        assert_eq!(free_tag_color("gear"), free_tag_color("gear"));
        let spread: std::collections::HashSet<TagColor> = [
            "rust", "gear", "travel", "vlog", "review", "setup", "japan", "ai",
        ]
        .iter()
        .map(|tag| free_tag_color(tag))
        .collect();
        assert!(
            spread.len() > 3,
            "tags should not collapse onto a few colors"
        );
    }
}
