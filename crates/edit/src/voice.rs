//! A speaker's voice, cleaned before it is cut: the noise in and between the words
//! lowered by DeepFilterNet (a neural noise remover, run as its command-line program),
//! rumble under 80 Hz filtered out, and the channels joined into one centred voice.
//!
//! A take is cleaned once, whole, so every pause and join of the cut carries the same
//! quieter room sound, and kept as `edit/.cache/audio/<stem>.<clock>.voice-<how>.48k.pcm`
//! in the format of [`audio::render_audio`], so the renderer cuts from it the same way.
//! DeepFilterNet keeps the timing to the sample (`--compensate-delay`); a long take is
//! cleaned in pieces, several at once, that overlap and are faded into each other.

use std::fs;
use std::io::{BufWriter, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::audio::{self, CLOCK_VERSION};
use crate::error::{EditError, Result};
use crate::project::{Project, Take};
use crate::tool;

const RATE: usize = 48_000;
/// Most the noise is lowered. Some of the room stays, so pauses don't sound dead and
/// quiet syllables aren't eaten.
pub const NOISE_REDUCTION_DB: u32 = 15;
pub const HIGH_PASS_HZ: f64 = 80.0;
/// A take is cleaned in pieces this long.
const PIECE: usize = 60 * RATE;
/// Sound before a piece that the noise remover hears first and that is then dropped, so
/// it has settled on the room's noise when the piece begins.
const LEAD: usize = RATE;
/// Neighbouring pieces overlap this long (20 ms) and are faded into each other.
const SEAM: usize = 960;
/// Silence after each piece: DeepFilterNet's output ends about 30 ms before its input.
const TAIL: usize = 4_800;

const DEEP_FILTER_VERSION: &str = "0.5.6";
/// DeepFilterNet's release builds: (OS, architecture, build, SHA-256).
const DEEP_FILTER_BUILDS: [(&str, &str, &str, &str); 5] = [
    (
        "linux",
        "x86_64",
        "x86_64-unknown-linux-musl",
        "70775e251eee44c0f2451a1e833326cf8bcbbe304d3e7cd12851e6fce72ef7da",
    ),
    (
        "linux",
        "aarch64",
        "aarch64-unknown-linux-gnu",
        "14e02a1c0028f3ca0bdf83b62b3336e56ba0556894ef295a95e8573f06557166",
    ),
    ("macos", "aarch64", "aarch64-apple-darwin", "4601e7f4e4c03e59a4c5b5000216ef3add3e808799cfccd95e14e83ea4611081"),
    ("macos", "x86_64", "x86_64-apple-darwin", "d3be84003acb7c23e738ad7f70a158ec779a8d233a82e7fa3e717d112eb5b50f"),
    (
        "windows",
        "x86_64",
        "x86_64-pc-windows-msvc.exe",
        "75e11fa16445f560cb6b021521ddb89e89270d13b83089705d98776f58fd7915",
    ),
];

/// DeepFilterNet's program: `deep-filter` on `PATH`, or the release build Rushes
/// downloads once into the user's cache folder, checked against its SHA-256.
pub fn deep_filter(on_progress: &(dyn Fn(&str) + Sync)) -> Result<PathBuf> {
    if let Some(path) = tool::find("deep-filter") {
        return Ok(path);
    }
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let (_, _, build, sha) = DEEP_FILTER_BUILDS
        .iter()
        .find(|(build_os, build_arch, ..)| *build_os == os && *build_arch == arch)
        .ok_or_else(|| EditError::invalid(format!("DeepFilterNet has no build for {os} on {arch}")))?;
    let dir =
        dirs::cache_dir().ok_or_else(|| EditError::invalid("no cache folder to keep DeepFilterNet in"))?.join("rushes");
    let name = format!("deep-filter-{DEEP_FILTER_VERSION}{}", std::env::consts::EXE_SUFFIX);
    let path = dir.join(&name);
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(&dir)?;
    on_progress("downloading DeepFilterNet, the noise remover (once, about 35 MB)");
    let url = format!(
        "https://github.com/Rikorose/DeepFilterNet/releases/download/v{DEEP_FILTER_VERSION}/deep-filter-{DEEP_FILTER_VERSION}-{build}"
    );
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(600))).build().new_agent();
    let response = agent
        .get(&url)
        .call()
        .map_err(|error| EditError::invalid(format!("couldn't download DeepFilterNet: {error}")))?;
    let part = dir.join(format!("{name}.{}.part", std::process::id()));
    let digest = (|| -> Result<String> {
        let mut body = response.into_body().into_reader();
        let mut file = fs::File::create(&part)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = body.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])?;
        }
        file.sync_all()?;
        Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
    })();
    match digest {
        Ok(digest) if digest == *sha => {}
        Ok(_) => {
            let _ = fs::remove_file(&part);
            return Err(EditError::invalid("the DeepFilterNet download doesn't match its checksum"));
        }
        Err(error) => {
            let _ = fs::remove_file(&part);
            return Err(EditError::invalid(format!("couldn't download DeepFilterNet: {error}")));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&part, fs::Permissions::from_mode(0o755))?;
    }
    fs::rename(&part, &path)?;
    Ok(path)
}

/// The take's sound with its voice cleaned, made once and reused while the take is
/// unchanged. With `denoiser` (DeepFilterNet's program) the noise is lowered too;
/// without, the voice is only filtered and centred.
pub fn cleaned_audio(
    project: &Project,
    take: &Take,
    denoiser: Option<&Path>,
    on_progress: &(dyn Fn(&str) + Sync),
) -> Result<PathBuf> {
    let path = cleaned_path(project, take, denoiser.is_some())?;
    if is_fresh(&path, take) {
        return Ok(path);
    }
    let source = audio::render_audio(project, take)?;
    let tmp = path.with_extension("tmp.pcm");
    let made = match denoiser {
        Some(program) => {
            on_progress(&format!("lowering the noise in {} (once per take)", take.file));
            let work = path.with_extension("work");
            let made = denoise(program, &source, &work, &tmp);
            let _ = fs::remove_dir_all(&work);
            made
        }
        None => filter_only(&source, &tmp),
    };
    if let Err(error) = made {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    fs::rename(&tmp, &path)?;
    if denoiser.is_some() {
        // A copy made earlier without DeepFilterNet isn't needed any more.
        let _ = fs::remove_file(cleaned_path(project, take, false)?);
    }
    Ok(path)
}

/// The cleaned sound of the take with its noise lowered, if it was made already.
pub fn denoised_audio(project: &Project, take: &Take) -> Result<Option<PathBuf>> {
    let path = cleaned_path(project, take, true)?;
    Ok(is_fresh(&path, take).then_some(path))
}

fn cleaned_path(project: &Project, take: &Take, denoised: bool) -> Result<PathBuf> {
    let how = if denoised { format!("dfn{NOISE_REDUCTION_DB}") } else { "hp".to_owned() };
    project.edit_path(&format!(".cache/audio/{}.{CLOCK_VERSION}.voice-{how}.48k.pcm", take.stem()))
}

fn is_fresh(path: &Path, take: &Take) -> bool {
    match (
        fs::metadata(path).and_then(|meta| meta.modified()),
        fs::metadata(&take.path).and_then(|meta| meta.modified()),
    ) {
        (Ok(copy), Ok(source)) => copy >= source,
        _ => false,
    }
}

/// One piece of a take: the samples it contributes, and the wider stretch the noise
/// remover is given for them.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Piece {
    /// First sample this piece contributes.
    start: usize,
    /// End of what it contributes, without the seam into the next piece.
    end: usize,
    /// Stretch given to the noise remover: from [`LEAD`] before `start` to [`SEAM`] after
    /// `end` (where the take has them).
    from: usize,
    to: usize,
}

fn pieces(frames: usize) -> Vec<Piece> {
    (0..frames.div_ceil(PIECE))
        .map(|index| {
            let (start, end) = (index * PIECE, ((index + 1) * PIECE).min(frames));
            Piece { start, end, from: start.saturating_sub(LEAD), to: (end + SEAM).min(frames) }
        })
        .collect()
}

/// Lowers the noise of the stereo PCM `source` into `out`: its pieces, centred to one
/// channel, go through DeepFilterNet in parallel, then they are joined in order,
/// filtered and written as stereo again.
fn denoise(program: &Path, source: &Path, work: &Path, out: &Path) -> Result<()> {
    let frames = (fs::metadata(source)?.len() / 4) as usize;
    let pieces = pieces(frames);
    let (inputs, outputs) = (work.join("in"), work.join("out"));
    fs::create_dir_all(&inputs)?;
    fs::create_dir_all(&outputs)?;
    // DeepFilterNet runs on one thread, and more of them than half the processors clean no
    // faster (they wait on memory), so the rest are left for the app.
    let workers = (std::thread::available_parallelism().map_or(4, |n| n.get()) / 2).clamp(1, pieces.len().max(1));
    let next = AtomicUsize::new(0);
    let error: Mutex<Option<EditError>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut file = match fs::File::open(source) {
                    Ok(file) => file,
                    Err(err) => {
                        error.lock().unwrap().get_or_insert(err.into());
                        return;
                    }
                };
                loop {
                    if error.lock().unwrap().is_some() {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(piece) = pieces.get(index) else { break };
                    let input = inputs.join(format!("{index:05}.wav"));
                    let done = read_mono(&mut file, piece.from, piece.to - piece.from)
                        .and_then(|mut samples| {
                            samples.resize(samples.len() + TAIL, 0);
                            write_wav(&input, &samples)
                        })
                        .and_then(|()| {
                            tool::run(
                                program,
                                [
                                    "--compensate-delay".as_ref(),
                                    "--atten-lim-db".as_ref(),
                                    NOISE_REDUCTION_DB.to_string().as_ref(),
                                    "--output-dir".as_ref(),
                                    outputs.as_os_str(),
                                    input.as_os_str(),
                                ],
                            )
                            .map(drop)
                        });
                    let _ = fs::remove_file(&input);
                    if let Err(err) = done {
                        error.lock().unwrap().get_or_insert(err);
                        break;
                    }
                }
            });
        }
    });
    if let Some(error) = error.into_inner().unwrap() {
        return Err(error);
    }
    let mut writer = VoiceWriter::create(out)?;
    let mut seam: Vec<i16> = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let output = outputs.join(format!("{index:05}.wav"));
        let mut cleaned = audio::read_wav(&output)?;
        let _ = fs::remove_file(&output);
        // Missing samples (an output a little short) are silence.
        cleaned.resize(piece.to - piece.from, 0);
        let own = &cleaned[piece.start - piece.from..];
        let (own, next_seam) = own.split_at(piece.end - piece.start);
        let faded = seam.len().min(own.len());
        for (at, (previous, current)) in seam.iter().zip(own).enumerate() {
            let weight = (at as f32 + 0.5) / faded as f32;
            writer.push(f32::from(*previous) * (1.0 - weight) + f32::from(*current) * weight)?;
        }
        for sample in &own[faded..] {
            writer.push(f32::from(*sample))?;
        }
        seam = next_seam.to_vec();
    }
    writer.finish()
}

/// Filters and centres the stereo PCM `source` into `out`, without lowering the noise.
fn filter_only(source: &Path, out: &Path) -> Result<()> {
    let mut file = fs::File::open(source)?;
    let frames = (fs::metadata(source)?.len() / 4) as usize;
    let mut writer = VoiceWriter::create(out)?;
    let mut at = 0;
    while at < frames {
        let count = PIECE.min(frames - at);
        for sample in read_mono(&mut file, at, count)? {
            writer.push(f32::from(sample))?;
        }
        at += count;
    }
    writer.finish()
}

/// `count` frames of 48 kHz stereo 16-bit PCM from frame `first`, as one channel (the
/// average of the two).
fn read_mono(file: &mut fs::File, first: usize, count: usize) -> Result<Vec<i16>> {
    file.seek(SeekFrom::Start(first as u64 * 4))?;
    let mut bytes = vec![0u8; count * 4];
    let mut filled = 0;
    while filled < bytes.len() {
        let read = file.read(&mut bytes[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(bytes[..filled - filled % 4]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|frame| {
            let left = i32::from(i16::from_le_bytes([frame[0], frame[1]]));
            let right = i32::from(i16::from_le_bytes([frame[2], frame[3]]));
            ((left + right) / 2) as i16
        })
        .collect())
}

/// A 48 kHz mono 16-bit WAV file.
fn write_wav(path: &Path, samples: &[i16]) -> Result<()> {
    let data = u32::try_from(samples.len() * 2).map_err(|_| EditError::invalid("audio piece too long"))?;
    let mut out = BufWriter::new(fs::File::create(path)?);
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&1u16.to_le_bytes())?; // mono
    out.write_all(&(RATE as u32).to_le_bytes())?;
    out.write_all(&(RATE as u32 * 2).to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data.to_le_bytes())?;
    for sample in samples {
        out.write_all(&sample.to_le_bytes())?;
    }
    out.flush()?;
    Ok(())
}

/// Writes the cleaned voice: high-passed, as stereo 16-bit PCM.
struct VoiceWriter {
    out: BufWriter<fs::File>,
    high_pass: [Biquad; 2],
}

impl VoiceWriter {
    fn create(path: &Path) -> Result<Self> {
        let section = Biquad::high_pass(HIGH_PASS_HZ, RATE as f64);
        Ok(Self { out: BufWriter::new(fs::File::create(path)?), high_pass: [section.clone(), section] })
    }

    /// One sample, on the 16-bit scale.
    fn push(&mut self, sample: f32) -> Result<()> {
        let filtered = self.high_pass.iter_mut().fold(f64::from(sample), |value, section| section.process(value));
        let bytes = (filtered.round().clamp(-32768.0, 32767.0) as i16).to_le_bytes();
        self.out.write_all(&bytes)?;
        self.out.write_all(&bytes)?;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.out.flush()?;
        Ok(())
    }
}

/// A second-order filter section (transposed direct form II).
#[derive(Clone, Debug)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    state: [f64; 2],
}

impl Biquad {
    /// Butterworth high-pass (Q = 1/√2), from the Audio EQ Cookbook.
    fn high_pass(cutoff: f64, rate: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * cutoff / rate;
        let alpha = w0.sin() / std::f64::consts::SQRT_2;
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b: [(1.0 + cos) / 2.0 / a0, -(1.0 + cos) / a0, (1.0 + cos) / 2.0 / a0],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            state: [0.0; 2],
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.state[0];
        self.state[0] = self.b[1] * x - self.a[0] * y + self.state[1];
        self.state[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frequency: f64, seconds: f64) -> Vec<f64> {
        (0..(seconds * RATE as f64) as usize)
            .map(|n| (2.0 * std::f64::consts::PI * frequency * n as f64 / RATE as f64).sin())
            .collect()
    }

    fn level_after_high_pass(frequency: f64) -> f64 {
        let section = Biquad::high_pass(HIGH_PASS_HZ, RATE as f64);
        let mut sections = [section.clone(), section];
        let out: Vec<f64> = tone(frequency, 1.0)
            .into_iter()
            .map(|x| sections.iter_mut().fold(x, |value, section| section.process(value)))
            .collect();
        // RMS of the second half (past the filter's settling), relative to the tone's.
        let tail = &out[out.len() / 2..];
        20.0 * ((tail.iter().map(|v| v * v).sum::<f64>() / tail.len() as f64).sqrt() / std::f64::consts::FRAC_1_SQRT_2)
            .log10()
    }

    #[test]
    fn high_pass_removes_rumble_and_keeps_the_voice() {
        assert!(level_after_high_pass(30.0) < -30.0, "{}", level_after_high_pass(30.0));
        assert!((level_after_high_pass(80.0) + 6.0).abs() < 0.5, "{}", level_after_high_pass(80.0));
        assert!(level_after_high_pass(300.0).abs() < 0.2, "{}", level_after_high_pass(300.0));
        assert!(level_after_high_pass(3000.0).abs() < 0.05);
    }

    #[test]
    fn pieces_cover_the_take_once_with_lead_and_seam() {
        let frames = 2 * PIECE + 1234;
        let pieces = pieces(frames);
        assert_eq!(pieces.len(), 3);
        assert_eq!((pieces[0].from, pieces[0].start), (0, 0));
        for pair in pieces.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
            assert_eq!(pair[0].to, pair[0].end + SEAM);
            assert_eq!(pair[1].from, pair[1].start - LEAD);
        }
        assert_eq!((pieces[2].end, pieces[2].to), (frames, frames));
    }

    /// With a stand-in noise remover that hands back its input, a take comes out as its
    /// centred, filtered self: every piece in place, the seams invisible.
    #[cfg(unix)]
    #[test]
    fn pieces_are_joined_back_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("deep-filter");
        // deep-filter [options] --output-dir <dir> <file>: copies <file> into <dir>.
        fs::write(&program, "#!/bin/sh\nwhile [ \"$1\" != --output-dir ]; do shift; done\ncp \"$3\" \"$2/\"\n")
            .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        // 2.5 pieces of a 440 Hz tone on the left, silence on the right.
        let frames = 2 * PIECE + PIECE / 2;
        let left: Vec<i16> = tone(440.0, frames as f64 / RATE as f64).iter().map(|v| (v * 16000.0) as i16).collect();
        let source = dir.path().join("take.pcm");
        let bytes: Vec<u8> =
            left.iter().flat_map(|sample| [sample.to_le_bytes(), 0i16.to_le_bytes()].concat()).collect();
        fs::write(&source, bytes).unwrap();

        let (denoised, filtered) = (dir.path().join("denoised.pcm"), dir.path().join("filtered.pcm"));
        denoise(&program, &source, &dir.path().join("work"), &denoised).unwrap();
        filter_only(&source, &filtered).unwrap();
        let (denoised, filtered) = (fs::read(denoised).unwrap(), fs::read(filtered).unwrap());
        assert_eq!(denoised.len(), frames * 4);
        // The same as filtering alone, but for rounding at the seams.
        let samples = |bytes: &[u8]| -> Vec<i16> {
            bytes.as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair)).collect()
        };
        let worst = samples(&denoised).iter().zip(samples(&filtered)).map(|(a, b)| (a - b).abs()).max().unwrap();
        assert!(worst <= 2, "{worst}");
        // Centred: both channels carry half of the left (past the filter's settling).
        let out = samples(&filtered);
        assert_eq!(out[PIECE * 2], out[PIECE * 2 + 1]);
        let peak = out[RATE * 2..].iter().map(|sample| sample.unsigned_abs()).max().unwrap();
        assert!((7900..=8100).contains(&peak), "{peak}");
    }
}
