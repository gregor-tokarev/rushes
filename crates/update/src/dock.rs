//! The Dock keeps an app by its path and name: it shows a kept app that was
//! renamed or replaced under its old name, or beside a second icon. Rushes
//! points the Dock's VideoCRM icon at `Rushes.app` the way `defaults` edits
//! the Dock's preferences, then restarts the Dock to show it.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Points the Dock's kept icons for the app at `from` at the app `to`, under
/// `to`'s name, in their place. Returns whether there was one.
pub(crate) fn replace(from: &Path, to: &Path, identifier: &str) -> io::Result<bool> {
    let from_url = file_url(from);
    // Read straight from the file, which costs no process: the Dock saves a
    // change right away, and an icon left behind stays for good.
    let Some(home) = env::var_os("HOME") else {
        return Ok(false);
    };
    let saved = fs::read(PathBuf::from(home).join("Library/Preferences/com.apple.dock.plist")).unwrap_or_default();
    if !saved.windows(from_url.len()).any(|window| window == from_url.as_bytes()) {
        return Ok(false);
    }

    let export = env::temp_dir().join(format!("rushes-dock-{}.plist", std::process::id()));
    let result = run(Command::new("/usr/bin/defaults").args(["export", "com.apple.dock"]).arg(&export))
        .and_then(|()| replace_tiles(&export, &from_url, to, identifier))
        .and_then(|replaced| {
            if replaced {
                run(Command::new("/usr/bin/defaults").args(["import", "com.apple.dock"]).arg(&export))?;
                // launchd starts the Dock again, which reads its preferences anew.
                run(Command::new("/usr/bin/killall").arg("Dock"))?;
            }
            Ok(replaced)
        });
    _ = fs::remove_file(&export);
    result
}

/// Edits the tiles for `from_url` in `export`, a copy of the Dock's preferences.
fn replace_tiles(export: &Path, from_url: &str, to: &Path, identifier: &str) -> io::Result<bool> {
    let label = to.file_stem().unwrap_or_default().to_string_lossy();
    let mut replaced = false;
    // Until the index runs past the list.
    for index in 0.. {
        let tile = format!(":persistent-apps:{index}:tile-data");
        if plist_buddy(export, &format!("Print :persistent-apps:{index}")).is_err() {
            break;
        }
        // A spacer has no URL.
        let url = plist_buddy(export, &format!("Print {tile}:file-data:_CFURLString")).unwrap_or_default();
        if url.trim() != from_url {
            continue;
        }
        plist_buddy(export, &format!("Set {tile}:file-data:_CFURLString {}", file_url(to)))?;
        set_string(export, &format!("{tile}:file-label"), &label)?;
        set_string(export, &format!("{tile}:bundle-identifier"), identifier)?;
        // Its bookmark finds the old app; the Dock makes one for the new path.
        _ = plist_buddy(export, &format!("Delete {tile}:book"));
        replaced = true;
    }
    Ok(replaced)
}

/// The URL the Dock keeps for the app bundle at `path`.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url.push('/');
    url
}

fn set_string(file: &Path, key: &str, value: &str) -> io::Result<()> {
    plist_buddy(file, &format!("Set {key} {value}"))
        .or_else(|_| plist_buddy(file, &format!("Add {key} string {value}")))
        .map(drop)
}

fn plist_buddy(file: &Path, command: &str) -> io::Result<String> {
    let output = Command::new("/usr/libexec/PlistBuddy").arg("-c").arg(command).arg(file).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(io::Error::other(format!("PlistBuddy {command}: {}", String::from_utf8_lossy(&output.stdout).trim())))
    }
}

fn run(command: &mut Command) -> io::Result<()> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    let program = Path::new(command.get_program()).file_name().unwrap_or_default().to_string_lossy().into_owned();
    Err(io::Error::other(format!(
        "{program} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_an_app_like_the_dock() {
        assert_eq!(file_url(Path::new("/Applications/VideoCRM.app")), "file:///Applications/VideoCRM.app/");
        assert_eq!(file_url(Path::new("/Users/me/My Apps/Rushes.app")), "file:///Users/me/My%20Apps/Rushes.app/");
    }

    #[test]
    fn edits_only_the_icon_of_the_old_app() {
        let dir = tempfile::tempdir().expect("temp dir");
        let export = dir.path().join("dock.plist");
        fs::write(&export, PLIST).expect("plist");
        let from = file_url(Path::new("/Applications/VideoCRM.app"));
        let to = Path::new("/Applications/Rushes.app");
        assert!(replace_tiles(&export, &from, to, "work.tokarev.rushes").expect("edit"));

        let tile = |index: usize, key: &str| {
            plist_buddy(&export, &format!("Print :persistent-apps:{index}:tile-data:{key}"))
                .map(|out| out.trim().to_owned())
        };
        assert_eq!(tile(0, "file-label").expect("label"), "Safari");
        assert_eq!(tile(2, "file-data:_CFURLString").expect("url"), "file:///Applications/Rushes.app/");
        assert_eq!(tile(2, "file-label").expect("label"), "Rushes");
        assert_eq!(tile(2, "bundle-identifier").expect("id"), "work.tokarev.rushes");
        assert!(tile(2, "book").is_err(), "the bookmark of the old app goes");
        assert!(!replace_tiles(&export, &from, to, "work.tokarev.rushes").expect("edit"), "nothing left");
    }

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>persistent-apps</key>
  <array>
    <dict>
      <key>tile-data</key>
      <dict>
        <key>bundle-identifier</key><string>com.apple.Safari</string>
        <key>file-data</key>
        <dict><key>_CFURLString</key><string>file:///Applications/Safari.app/</string><key>_CFURLStringType</key><integer>15</integer></dict>
        <key>file-label</key><string>Safari</string>
      </dict>
    </dict>
    <dict>
      <key>tile-data</key><dict/>
      <key>tile-type</key><string>spacer-tile</string>
    </dict>
    <dict>
      <key>tile-data</key>
      <dict>
        <key>book</key><data>Ym9vaw==</data>
        <key>file-data</key>
        <dict><key>_CFURLString</key><string>file:///Applications/VideoCRM.app/</string><key>_CFURLStringType</key><integer>15</integer></dict>
        <key>file-label</key><string>VideoCRM</string>
      </dict>
    </dict>
  </array>
</dict>
</plist>
"#;
}
