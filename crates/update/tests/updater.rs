//! The updater against a local stand-in for GitHub: checking, downloading,
//! verifying and installing release files into installations in temp folders.
//!
//! `installs_the_bundles_built_here` also installs real bundles: the Bundle
//! workflow points `VIDEOCRM_UPDATE_TEST_BUNDLES` at the folder it built them
//! in and `VIDEOCRM_UPDATE_TEST_VERSION` at their version.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use sha2::{Digest as _, Sha256};
use videocrm_update::{Delivery, Error, Installation, ManualReason, Progress, Updater, Version};

/// Status and body by request path.
type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>;

/// Answers like GitHub: the newest release at `/latest`, its files under `/download/`.
struct FakeGithub {
    base: String,
    routes: Routes,
}

impl FakeGithub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let routes = Routes::default();
        let served = routes.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                // The headers end at an empty line.
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                    line.clear();
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, body) = served
                    .lock()
                    .expect("routes")
                    .get(&path)
                    .cloned()
                    .unwrap_or((404, b"{\"message\":\"Not Found\"}".to_vec()));
                let head =
                    format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&body));
            }
        });
        Self { base, routes }
    }

    fn latest_url(&self) -> String {
        format!("{}/latest", self.base)
    }

    fn route(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.routes.lock().expect("routes").insert(path.to_owned(), (status, body.into()));
    }

    /// Publishes release `version` with `files`, listing `digests` for them
    /// (by default their real SHA-256).
    fn publish(&self, version: &str, files: &[(&str, Vec<u8>)], digests: &HashMap<&str, String>) {
        let assets: Vec<serde_json::Value> = files
            .iter()
            .map(|(name, bytes)| {
                let digest =
                    digests.get(name).cloned().unwrap_or_else(|| format!("sha256:{}", hex(&Sha256::digest(bytes))));
                serde_json::json!({
                    "name": name,
                    "size": bytes.len(),
                    "digest": digest,
                    "browser_download_url": format!("{}/download/{name}", self.base),
                })
            })
            .collect();
        let release = serde_json::json!({
            "tag_name": format!("v{version}"),
            "html_url": format!("https://github.com/gregor-tokarev/videocrm/releases/tag/v{version}"),
            "body": "Notes",
            "assets": assets,
        });
        self.route("/latest", 200, release.to_string());
        for (name, bytes) in files {
            self.route(&format!("/download/{name}"), 200, bytes.clone());
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn version(text: &str) -> Version {
    Version::parse(text).expect("version")
}

/// The first bytes of an AppImage (type 2), then `payload`.
fn appimage(payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"\x7fELF\x02\x01\x01\x00AI\x02".to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

/// A tarball shaped like `scripts/bundle-linux.sh` makes them.
fn tarball(version: &str, binary: &[u8]) -> Vec<u8> {
    let root = format!("videocrm-{version}-linux-{}", std::env::consts::ARCH);
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    let files: [(&str, &[u8], u32); 3] = [
        ("bin/videocrm", binary, 0o755),
        ("share/applications/videocrm.desktop", b"[Desktop Entry]\nName=VideoCRM\n", 0o644),
        ("share/icons/hicolor/scalable/apps/videocrm.svg", b"<svg/>", 0o644),
    ];
    for (path, bytes, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder.append_data(&mut header, format!("{root}/{path}"), bytes).expect("append");
    }
    builder.into_inner().expect("tar").finish().expect("gzip")
}

fn appimage_name(version: &str) -> String {
    format!("VideoCRM-{version}-{}.AppImage", std::env::consts::ARCH)
}

fn tarball_name(version: &str) -> String {
    format!("videocrm-{version}-linux-{}.tar.gz", std::env::consts::ARCH)
}

/// Runs the whole update as the app does; returns what progress reported.
fn update(updater: &Updater) -> Result<Vec<Progress>, Error> {
    let found = updater.check(updater.current())?.expect("a newer release");
    let Delivery::Install(asset) = found.delivery else { panic!("installs itself, got {:?}", found.delivery) };
    let mut progress = Vec::new();
    updater.install(&asset, &mut |step| progress.push(step), &AtomicBool::new(false))?;
    Ok(progress)
}

fn old_appimage(dir: &Path) -> PathBuf {
    let file = dir.join("VideoCRM.AppImage");
    fs::write(&file, appimage(b"old")).expect("old AppImage");
    file
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn nothing_to_do_when_the_newest_release_is_running() {
    let github = FakeGithub::start();
    github.publish("0.1.1", &[], &HashMap::new());
    let updater = Updater::new(github.latest_url(), version("0.1.1"), None);
    assert_eq!(updater.check(updater.current()).expect("check"), None);

    // A version installed since counts as running.
    github.publish("0.1.2", &[], &HashMap::new());
    assert_eq!(updater.check(&version("0.1.2")).expect("check"), None);
    let newer = updater.check(updater.current()).expect("check").expect("0.1.2");
    assert_eq!(newer.release.version, version("0.1.2"));
}

#[cfg(target_os = "linux")]
#[test]
fn replaces_the_appimage() {
    let github = FakeGithub::start();
    let new = appimage(&vec![7; 300 * 1024]);
    github.publish("0.2.0", &[(&appimage_name("0.2.0"), new.clone())], &HashMap::new());
    let dir = tempfile::tempdir().expect("temp dir");
    let file = old_appimage(dir.path());
    let updater =
        Updater::new(github.latest_url(), version("0.1.1"), Some(Installation::AppImage { file: file.clone() }));

    let progress = update(&updater).expect("update");

    assert_eq!(fs::read(&file).expect("AppImage"), new);
    assert_eq!(mode(&file), 0o755);
    let total = new.len() as u64;
    assert_eq!(progress.first(), Some(&Progress::Downloading { received: 0, total }));
    assert!(progress.contains(&Progress::Downloading { received: total, total }));
    assert_eq!(progress.last(), Some(&Progress::Installing));
    assert_eq!(fs::read_dir(dir.path()).expect("folder").count(), 1, "nothing left beside the AppImage");
}

#[cfg(target_os = "linux")]
#[test]
fn replaces_the_tarball_in_its_prefix() {
    let github = FakeGithub::start();
    github.publish("0.2.0", &[(&tarball_name("0.2.0"), tarball("0.2.0", b"new binary"))], &HashMap::new());
    let prefix = tempfile::tempdir().expect("temp dir");
    fs::create_dir_all(prefix.path().join("bin")).expect("bin");
    fs::create_dir_all(prefix.path().join("share/applications")).expect("applications");
    fs::write(prefix.path().join("bin/videocrm"), b"old binary").expect("old binary");
    fs::write(prefix.path().join("bin/other-tool"), b"someone else's").expect("other");
    fs::write(prefix.path().join("share/applications/videocrm.desktop"), b"old").expect("old entry");
    let detected = Installation::detect_from(&prefix.path().join("bin/videocrm"), None, None);
    assert_eq!(detected, Some(Installation::Prefix { prefix: prefix.path().to_path_buf() }));
    let updater = Updater::new(github.latest_url(), version("0.1.1"), detected);

    update(&updater).expect("update");

    let read = |path: &str| fs::read(prefix.path().join(path)).expect(path);
    assert_eq!(read("bin/videocrm"), b"new binary");
    assert_eq!(mode(&prefix.path().join("bin/videocrm")), 0o755);
    assert_eq!(read("share/applications/videocrm.desktop"), b"[Desktop Entry]\nName=VideoCRM\n");
    assert_eq!(read("share/icons/hicolor/scalable/apps/videocrm.svg"), b"<svg/>");
    assert_eq!(read("bin/other-tool"), b"someone else's");
    assert!(!prefix.path().join(".videocrm-update").exists(), "the staging folder is gone");
}

#[cfg(target_os = "linux")]
#[test]
fn keeps_the_old_version_when_the_download_is_damaged() {
    let github = FakeGithub::start();
    let name = appimage_name("0.2.0");
    let digests = HashMap::from([(name.as_str(), format!("sha256:{}", "00".repeat(32)))]);
    github.publish("0.2.0", &[(&name, appimage(b"new"))], &digests);
    let dir = tempfile::tempdir().expect("temp dir");
    let file = old_appimage(dir.path());
    let updater =
        Updater::new(github.latest_url(), version("0.1.1"), Some(Installation::AppImage { file: file.clone() }));

    let error = update(&updater).expect_err("damaged");

    assert!(matches!(error, Error::Corrupt(_)), "{error}");
    assert_eq!(fs::read(&file).expect("AppImage"), appimage(b"old"));
    assert_eq!(fs::read_dir(dir.path()).expect("folder").count(), 1, "the download is gone");

    // Not an AppImage at all, though GitHub vouches for it.
    github.publish("0.2.0", &[(&name, b"<html>".to_vec())], &HashMap::new());
    assert!(matches!(update(&updater), Err(Error::Corrupt(_))));
    assert_eq!(fs::read(&file).expect("AppImage"), appimage(b"old"));
}

#[test]
fn copies_that_cant_replace_themselves_download_by_hand() {
    let github = FakeGithub::start();
    github.publish("0.2.0", &[(&appimage_name("0.2.0"), appimage(b"new"))], &HashMap::new());

    let built = Updater::new(github.latest_url(), version("0.1.1"), None);
    let found = built.check(built.current()).expect("check").expect("newer");
    assert_eq!(found.delivery, Delivery::Manual(ManualReason::NotInstalled));
    assert_eq!(found.release.page_url, "https://github.com/gregor-tokarev/videocrm/releases/tag/v0.2.0");

    let dir = tempfile::tempdir().expect("temp dir");
    let mac = Installation::MacApp { bundle: dir.path().join("VideoCRM.app") };
    let without_file = Updater::new(github.latest_url(), version("0.1.1"), Some(mac));
    let found = without_file.check(without_file.current()).expect("check").expect("newer");
    assert_eq!(found.delivery, Delivery::Manual(ManualReason::NoFile));
}

#[cfg(unix)]
#[test]
fn a_folder_of_someone_else_downloads_by_hand() {
    use std::os::unix::fs::PermissionsExt as _;

    let github = FakeGithub::start();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = old_appimage(dir.path());
    // The release file for this machine's kind of installation.
    let installation = Installation::AppImage { file };
    let name = installation.asset_name(&version("0.2.0"));
    github.publish("0.2.0", &[(&name, appimage(b"new"))], &HashMap::new());
    let updater = Updater::new(github.latest_url(), version("0.1.1"), Some(installation));

    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).expect("read-only");
    let found = updater.check(updater.current());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).expect("writable again");
    let delivery = found.expect("check").expect("newer").delivery;
    // Root writes anywhere.
    if !matches!(delivery, Delivery::Install(_)) {
        assert_eq!(delivery, Delivery::Manual(ManualReason::ReadOnly(dir.path().to_path_buf())));
    }
}

#[test]
fn says_when_github_limits_checks() {
    let github = FakeGithub::start();
    github.route("/latest", 403, r#"{"message":"API rate limit exceeded for 127.0.0.1."}"#);
    let updater = Updater::new(github.latest_url(), version("0.1.1"), None);
    let error = updater.check(updater.current()).expect_err("limited");
    assert!(error.is_rate_limited(), "{error}");
    assert_eq!(error.to_string(), "GitHub answered 403: API rate limit exceeded for 127.0.0.1.");

    let unreachable = Updater::new("http://127.0.0.1:1/latest", version("0.1.1"), None);
    assert!(unreachable.check(unreachable.current()).expect_err("no server").is_network());
}

/// Installs the bundles in `$VIDEOCRM_UPDATE_TEST_BUNDLES` over an older
/// installation of each kind this system has.
#[test]
fn installs_the_bundles_built_here() {
    let (Some(dir), Some(built)) =
        (std::env::var_os("VIDEOCRM_UPDATE_TEST_BUNDLES"), std::env::var("VIDEOCRM_UPDATE_TEST_VERSION").ok())
    else {
        return;
    };
    let dir = PathBuf::from(dir);
    let built = version(&built);
    let home = tempfile::tempdir().expect("temp dir");
    let mut installations = Vec::new();
    if cfg!(target_os = "macos") {
        let bundle = home.path().join("Applications/VideoCRM.app");
        fs::create_dir_all(bundle.join("Contents/MacOS")).expect("old bundle");
        fs::write(bundle.join("Contents/MacOS/videocrm"), b"#!/bin/sh\n").expect("old executable");
        installations.push(Installation::MacApp { bundle });
    } else {
        fs::create_dir_all(home.path().join("Apps")).expect("apps");
        installations.push(Installation::AppImage { file: old_appimage(&home.path().join("Apps")) });
        let prefix = home.path().join(".local");
        fs::create_dir_all(prefix.join("bin")).expect("bin");
        fs::create_dir_all(prefix.join("share/applications")).expect("applications");
        fs::write(prefix.join("bin/videocrm"), b"old").expect("old binary");
        fs::write(prefix.join("share/applications/videocrm.desktop"), b"old").expect("old entry");
        installations.push(Installation::Prefix { prefix });
    }

    for installation in installations {
        let github = FakeGithub::start();
        let name = installation.asset_name(&built);
        let bytes = fs::read(dir.join(&name)).unwrap_or_else(|error| panic!("{name} in {}: {error}", dir.display()));
        github.publish(&built.to_string(), &[(&name, bytes)], &HashMap::new());
        let updater = Updater::new(github.latest_url(), version("0.0.1"), Some(installation.clone()));

        update(&updater).unwrap_or_else(|error| panic!("installing {name}: {error}"));

        match &installation {
            Installation::MacApp { bundle } => {
                let plist = fs::read_to_string(bundle.join("Contents/Info.plist")).expect("Info.plist");
                assert!(plist.contains(&format!("<string>{built}</string>")), "{plist}");
                let verified = std::process::Command::new("codesign")
                    .args(["--verify", "--strict"])
                    .arg(bundle)
                    .status()
                    .expect("codesign");
                assert!(verified.success(), "the installed bundle keeps its signature");
            }
            Installation::AppImage { file } => {
                assert_eq!(fs::read(file).expect("AppImage"), fs::read(dir.join(&name)).expect("bundle"));
            }
            Installation::Prefix { prefix } => {
                assert_eq!(mode(&prefix.join("bin/videocrm")), 0o755);
                let entry = fs::read_to_string(prefix.join("share/applications/videocrm.desktop")).expect("entry");
                assert!(entry.contains("StartupWMClass=videocrm"), "{entry}");
            }
        }
        assert!(!home.path().join("Applications/.videocrm-update").exists());
    }
}
