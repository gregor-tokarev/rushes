use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest as _, Sha256};
use ureq::Agent;

use crate::error::{Error, Result};
use crate::release::Asset;

/// Reports how many bytes of how many arrived.
pub type DownloadProgress<'a> = &'a mut dyn FnMut(u64, u64);

/// Downloads `asset` to `path` and checks it's the file the release lists:
/// its size, and its SHA-256 when GitHub knows it.
pub(crate) fn download(
    agent: &Agent,
    asset: &Asset,
    path: &Path,
    progress: DownloadProgress<'_>,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut response = agent.get(&asset.url).header("Accept", "application/octet-stream").call()?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(Error::Status { status, message: String::new() });
    }
    let mut file = File::create(path).map_err(|error| Error::io("cannot save the download", error))?;
    let mut reader = response.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut received = 0;
    let mut buffer = vec![0; 64 * 1024];
    progress(0, asset.size);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let read = reader.read(&mut buffer).map_err(|error| Error::io("the download stopped", error))?;
        if read == 0 {
            break;
        }
        received += read as u64;
        if received > asset.size {
            return Err(Error::Corrupt(format!("it's larger than the {} bytes the release lists", asset.size)));
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(|error| Error::io("cannot save the download", error))?;
        progress(received, asset.size);
    }
    file.sync_all().map_err(|error| Error::io("cannot save the download", error))?;
    if received != asset.size {
        return Err(Error::Corrupt(format!("{received} of {} bytes arrived", asset.size)));
    }
    if let Some(expected) = asset.sha256
        && hasher.finalize().as_slice() != expected
    {
        return Err(Error::Corrupt("its SHA-256 differs from the release's".into()));
    }
    Ok(())
}
