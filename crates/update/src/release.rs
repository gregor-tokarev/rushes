use serde::Deserialize;

use crate::error::{Error, Result};
use crate::version::Version;

/// A published Rushes release, as GitHub lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release's page on GitHub, where every file can be downloaded.
    pub page_url: String,
    /// The release notes (Markdown).
    pub notes: String,
    pub assets: Vec<Asset>,
}

/// One downloadable file of a [`Release`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// The SHA-256 GitHub computed when the file was uploaded.
    pub sha256: Option<[u8; 32]>,
}

impl Release {
    /// Reads GitHub's answer for a release (`GET /repos/{owner}/{repo}/releases/latest`).
    pub(crate) fn from_github_json(json: &str) -> Result<Self> {
        let release: GithubRelease =
            serde_json::from_str(json).map_err(|error| Error::Protocol(format!("not a release: {error}")))?;
        let version = Version::parse(&release.tag_name)
            .ok_or_else(|| Error::Protocol(format!("release tag {:?} isn't a version", release.tag_name)))?;
        let assets = release
            .assets
            .into_iter()
            .map(|asset| Asset {
                sha256: asset.digest.as_deref().and_then(parse_sha256),
                name: asset.name,
                url: asset.browser_download_url,
                size: asset.size,
            })
            .collect();
        Ok(Self { version, page_url: release.html_url, notes: release.body.unwrap_or_default(), assets })
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// `sha256:<hex>`; GitHub fills it for files uploaded since mid-2025.
    digest: Option<String>,
}

fn parse_sha256(digest: &str) -> Option<[u8; 32]> {
    let hex = digest.strip_prefix("sha256:")?;
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATEST: &str = r###"{
        "tag_name": "v0.1.1",
        "html_url": "https://github.com/gregor-tokarev/rushes/releases/tag/v0.1.1",
        "draft": false,
        "prerelease": false,
        "body": "## What's Changed\n* Several YouTube accounts",
        "assets": [
            {
                "name": "Rushes-0.1.1-x86_64.AppImage",
                "size": 24074744,
                "digest": "sha256:34a94cfe62144a560ec868b43232d4fed6e4158971a715fc66574023bff836c4",
                "browser_download_url": "https://github.com/gregor-tokarev/rushes/releases/download/v0.1.1/Rushes-0.1.1-x86_64.AppImage"
            },
            {
                "name": "rushes-0.1.1-linux-x86_64.tar.gz",
                "size": 25665743,
                "digest": null,
                "browser_download_url": "https://github.com/gregor-tokarev/rushes/releases/download/v0.1.1/rushes-0.1.1-linux-x86_64.tar.gz"
            }
        ]
    }"###;

    #[test]
    fn reads_githubs_latest_release() {
        let release = Release::from_github_json(LATEST).expect("release");
        assert_eq!(release.version, Version::new(0, 1, 1));
        assert_eq!(release.page_url, "https://github.com/gregor-tokarev/rushes/releases/tag/v0.1.1");
        assert!(release.notes.starts_with("## What's Changed"));

        let appimage = release.asset("Rushes-0.1.1-x86_64.AppImage").expect("AppImage");
        assert_eq!(appimage.size, 24074744);
        let sha256 = appimage.sha256.expect("digest");
        assert_eq!(&sha256[..4], &[0x34, 0xa9, 0x4c, 0xfe]);
        assert_eq!(sha256[31], 0xc4);
        assert_eq!(release.asset("rushes-0.1.1-linux-x86_64.tar.gz").expect("tarball").sha256, None);
        assert_eq!(release.asset("Rushes-0.1.1-macos-universal.dmg"), None);
    }

    #[test]
    fn rejects_answers_that_arent_releases() {
        assert!(matches!(Release::from_github_json(r#"{"message":"Not Found"}"#), Err(Error::Protocol(_))));
        let untagged = r#"{"tag_name":"nightly","html_url":"https://example.com","body":null}"#;
        assert!(matches!(Release::from_github_json(untagged), Err(Error::Protocol(_))));
    }

    #[test]
    fn reads_only_sha256_digests() {
        assert_eq!(parse_sha256(&format!("sha256:{}", "00".repeat(32))), Some([0; 32]));
        assert_eq!(parse_sha256(&format!("sha512:{}", "00".repeat(32))), None);
        assert_eq!(parse_sha256("sha256:abc"), None);
        assert_eq!(parse_sha256(&format!("sha256:{}", "zz".repeat(32))), None);
    }
}
