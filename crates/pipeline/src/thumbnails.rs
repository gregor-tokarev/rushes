//! YouTube thumbnails drawn by Codex (`codex exec`), whose image generation tool
//! makes the pictures, in a chat: every turn after the first resumes Codex's session
//! (`codex exec resume`), so it remembers what the creator asked and what it drew.
//!
//! Codex only draws and saves drafts as it is told; this module numbers them and
//! turns them into what YouTube takes (1280 × 720 JPG, well under 2 MB) with ffmpeg.
//! Like the edit agent, Codex runs headless in its own process group, its prompt on
//! stdin, and its `--json` events are reported as [`AgentEvent`]s.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::agent::{ProcessSpec, run_process};
use crate::error::PipelineError;
use crate::event::{AgentEvent, parse_codex_line};
use crate::exec::{find_executable, run_captured};
use crate::media::probe_media;

pub const DEFAULT_CODEX_COMMAND: &str = "codex";
/// How many thumbnails one turn of the chat may draw.
pub const MAX_THUMBNAILS: usize = 4;
/// How many ideas Codex draws when the creator doesn't say.
pub const DEFAULT_IDEAS: usize = 3;
/// The size YouTube recommends for thumbnails.
pub const THUMBNAIL_WIDTH: u32 = 1280;
pub const THUMBNAIL_HEIGHT: u32 = 720;

/// Codex's drafts and the reference frames, inside the thumbnails folder while it works.
const DRAFTS_DIR: &str = ".drafts";
const THUMBNAIL_PREFIX: &str = "thumbnail-";
/// The images the thumbnails folder lists, by extension.
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
/// How many of the newest thumbnails a later turn names.
const LISTED_THUMBNAILS: usize = 12;
/// What `codex exec resume` says when it has no session with the id.
const SESSION_NOT_FOUND: &str = "no rollout found";
/// Frames of the video Codex sees, so the people and the set stay recognisable.
const REFERENCE_FRAMES: usize = 3;
/// Longest stretch of the script Codex reads, in characters: enough to get the idea.
const SCRIPT_MAX_CHARS: usize = 6_000;
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(60);

/// How to launch Codex.
#[derive(Clone, PartialEq)]
pub struct CodexConfig {
    /// Executable: a bare name looked up on `PATH`, or a path.
    pub command: PathBuf,
    /// `--model`; `None` keeps Codex's own choice.
    pub model: Option<String>,
    /// `model_reasoning_effort`. Drawing needs little reasoning, and the creator's
    /// default may be the slowest; `None` keeps it.
    pub reasoning_effort: Option<String>,
    /// Extra environment for Codex and every tool it runs. A `PATH` entry here is
    /// also used to locate Codex and ffmpeg.
    pub env: Vec<(String, String)>,
    /// How long cancellation waits after SIGTERM before SIGKILL.
    pub kill_grace: Duration,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: PathBuf::from(DEFAULT_CODEX_COMMAND),
            model: None,
            reasoning_effort: Some("medium".to_owned()),
            env: Vec::new(),
            kill_grace: Duration::from_secs(5),
        }
    }
}

// `env` often carries API keys, so only the variable names are printed.
impl fmt::Debug for CodexConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env: Vec<&str> = self.env.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("CodexConfig")
            .field("command", &self.command)
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("env", &env)
            .field("kill_grace", &self.kill_grace)
            .finish()
    }
}

impl CodexConfig {
    /// Absolute path of the Codex executable, if it exists.
    pub fn resolve_command(&self) -> Option<PathBuf> {
        find_executable(&self.command, self.path_var().as_deref())
    }

    /// Codex's `PATH`: [`Self::env`] first, then ours.
    fn path_var(&self) -> Option<OsString> {
        self.env
            .iter()
            .rev()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| OsString::from(value))
            .or_else(|| std::env::var_os("PATH"))
    }

    /// `codex exec` in `cwd`, writing only there, with `images` attached to the
    /// prompt, which comes on stdin; with `resume`, `codex exec resume` of that
    /// session. The options with a value use the `--flag=value` form, and `--image`
    /// comes after them, so none can take another argument as its value; `--` keeps
    /// the session id from being read as an option.
    fn args(&self, cwd: &Path, images: &[PathBuf], resume: Option<&str>) -> Vec<OsString> {
        let mut args: Vec<OsString> =
            ["exec", "--json", "--skip-git-repo-check", "--sandbox=workspace-write"].map(OsString::from).to_vec();
        let mut cd = OsString::from("--cd=");
        cd.push(cwd);
        args.push(cd);
        if let Some(model) = &self.model {
            args.push(format!("--model={model}").into());
        }
        if let Some(effort) = &self.reasoning_effort {
            args.push(format!("--config=model_reasoning_effort=\"{effort}\"").into());
        }
        for image in images {
            let mut arg = OsString::from("--image=");
            arg.push(image);
            args.push(arg);
        }
        if let Some(session_id) = resume {
            args.extend(["resume", "--", session_id, "-"].map(OsString::from));
        }
        args
    }
}

/// One turn of a chat with Codex about a video's thumbnails: what the creator says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThumbnailRequest {
    /// The video's thumbnails folder, created when missing. Codex works and writes
    /// only there.
    pub dir: PathBuf,
    /// Codex's session of the chat, from the outcome of its last turn. `None` starts
    /// a new chat, which Codex begins with the video's title, script and frames.
    pub session_id: Option<String>,
    pub video_title: String,
    /// A YouTube Short rather than a long-form video.
    pub short: bool,
    /// The script as Markdown, for what the video is about; may be empty.
    pub script: String,
    /// What the creator says; empty asks for a few ideas.
    pub message: String,
    /// A video file to show Codex a few frames of when a chat starts.
    pub frames_from: Option<PathBuf>,
    /// The thumbnails Codex drew in the chat's last turn, as Rushes saved them, in
    /// the order it drew them.
    pub last_drawn: Vec<PathBuf>,
}

/// How a turn of the chat ended.
#[derive(Clone, Debug, PartialEq)]
pub struct ThumbnailOutcome {
    /// The new thumbnails, in the order Codex drew them; none when it only answered.
    pub made: Vec<PathBuf>,
    /// Codex's last message: one line per thumbnail it drew, or its answer.
    pub reply: String,
    /// Why Codex stopped before it finished, when it said.
    pub problem: Option<String>,
    /// Codex's session, for the chat's next turn.
    pub session_id: Option<String>,
}

/// Has Codex answer `request.message`, drawing as many thumbnails as it asks for (at
/// most [`MAX_THUMBNAILS`]), and puts them in `request.dir` as `thumbnail-<n>.jpg`,
/// numbered after the ones there. Codex keeps its own copies, and its session
/// remembers the chat for the next turn.
///
/// Blocks until Codex exits. Setting `cancel` stops it and every process it
/// started, and returns [`PipelineError::Cancelled`]; what it drew so far is dropped.
/// A session Codex no longer has is [`PipelineError::SessionNotFound`].
pub fn generate_thumbnails(
    config: &CodexConfig,
    request: &ThumbnailRequest,
    on_event: &mut dyn FnMut(&AgentEvent),
    cancel: &AtomicBool,
) -> Result<ThumbnailOutcome, PipelineError> {
    let program = config.resolve_command().ok_or_else(|| PipelineError::AgentNotFound(config.command.clone()))?;
    let session_id = request.session_id.as_deref().map(str::trim).filter(|id| !id.is_empty());
    let drafts_dir = request.dir.join(DRAFTS_DIR);
    // Drafts of a turn that never finished are of no use.
    if drafts_dir.exists() {
        fs::remove_dir_all(&drafts_dir)?;
    }
    fs::create_dir_all(&drafts_dir)?;
    // Codex runs where it may write, which `--cd` and the sandbox want as a real path.
    let dir = fs::canonicalize(&request.dir)?;
    let drafts_dir = dir.join(DRAFTS_DIR);
    let ffmpeg = find_executable(Path::new("ffmpeg"), config.path_var().as_deref());

    // A resumed session has seen the frames already.
    let frames = match (&request.frames_from, &ffmpeg, session_id) {
        (Some(video), Some(ffmpeg), None) => reference_frames(ffmpeg, video, &drafts_dir, &config.env),
        _ => Vec::new(),
    };
    let drafts: Vec<PathBuf> = (1..=MAX_THUMBNAILS).map(|n| drafts_dir.join(format!("draft-{n}.png"))).collect();
    let prompt = match session_id {
        None => thumbnail_prompt(request, &drafts, !frames.is_empty()),
        Some(_) => follow_up_prompt(request, &list_thumbnails(&dir), &drafts),
    };
    let spec = ProcessSpec {
        program: &program,
        args: config.args(&dir, &frames, session_id),
        cwd: &dir,
        env: &config.env,
        stdin: Some(prompt),
        kill_grace: config.kill_grace,
        parse: parse_codex_line,
    };
    let ran = run_process(&spec, on_event, cancel);
    let made = match &ran {
        Ok(_) => drafts.iter().filter_map(|draft| keep_draft(draft, &dir, ffmpeg.as_deref(), &config.env)).collect(),
        Err(_) => Vec::new(),
    };
    // Frames and drafts never stay; a leftover only costs disk.
    _ = fs::remove_dir_all(&drafts_dir);
    let (status, run) = ran?;

    if let Some(session_id) = session_id
        && run.result.is_none()
        && run.stderr_tail.iter().any(|line| line.contains(SESSION_NOT_FOUND))
    {
        return Err(PipelineError::SessionNotFound(session_id.to_owned()));
    }
    let reply = run.last_text.clone().unwrap_or_default();
    let reported = match &run.result {
        Some(AgentEvent::Result { is_error: true, result_text, .. }) => Some(result_text.clone()),
        Some(_) if !status.success() => Some(format!("Codex stopped unexpectedly ({status})")),
        Some(_) => None,
        None => Some(format!("Codex stopped without finishing ({status})")),
    };
    let session_id = run.session_id.clone().or_else(|| session_id.map(str::to_owned));
    // A turn that only answered draws nothing; one with nothing to show failed.
    let failure = match &reported {
        Some(message) if made.is_empty() => Some(message.clone()),
        None if made.is_empty() && reply.trim().is_empty() => Some("Codex finished without answering".to_owned()),
        _ => None,
    };
    match failure {
        Some(message) => Err(PipelineError::AgentFailed {
            message,
            stderr_tail: run.stderr_tail,
            session_id,
            no_result: run.result.is_none(),
        }),
        None => Ok(ThumbnailOutcome { made, reply, problem: reported, session_id }),
    }
}

/// The thumbnails drawn for a video, `thumbnail-<n>` images in `dir`, newest first.
pub fn list_thumbnails(dir: &Path) -> Vec<PathBuf> {
    let mut numbered: Vec<(u32, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            Some((thumbnail_number(&path)?, path))
        })
        .collect();
    numbered.sort_by(|a, b| b.cmp(a));
    numbered.into_iter().map(|(_, path)| path).collect()
}

/// `n` of a `thumbnail-<n>.<image extension>` file.
fn thumbnail_number(path: &Path) -> Option<u32> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    path.file_stem()?.to_str()?.strip_prefix(THUMBNAIL_PREFIX)?.parse().ok()
}

/// Turns Codex's draft into the next numbered thumbnail in `dir`: a 1280 × 720 JPG,
/// cropped to fill, or, without a working ffmpeg, the draft as it is. `None` when
/// there is no usable draft.
fn keep_draft(draft: &Path, dir: &Path, ffmpeg: Option<&Path>, env: &[(String, String)]) -> Option<PathBuf> {
    if fs::metadata(draft).ok().is_none_or(|meta| meta.len() == 0) {
        return None;
    }
    let next = list_thumbnails(dir).first().and_then(|path| thumbnail_number(path)).unwrap_or(0) + 1;
    let target = dir.join(format!("{THUMBNAIL_PREFIX}{next}.jpg"));
    let scale = format!(
        "scale={THUMBNAIL_WIDTH}:{THUMBNAIL_HEIGHT}:force_original_aspect_ratio=increase,\
         crop={THUMBNAIL_WIDTH}:{THUMBNAIL_HEIGHT}"
    );
    let converted = ffmpeg.is_some_and(|ffmpeg| {
        let args = [OsStr::new("-y"), OsStr::new("-loglevel"), OsStr::new("error"), OsStr::new("-i")]
            .into_iter()
            .chain([draft.as_os_str(), OsStr::new("-vf"), OsStr::new(&scale)])
            .chain([OsStr::new("-frames:v"), OsStr::new("1"), OsStr::new("-q:v"), OsStr::new("3")])
            .chain([target.as_os_str()]);
        run_captured(ffmpeg, args, env, FFMPEG_TIMEOUT).is_some() && target.is_file()
    });
    if converted {
        return Some(target);
    }
    _ = fs::remove_file(&target);
    let extension = draft.extension()?.to_str()?.to_ascii_lowercase();
    if !IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    let target = target.with_extension(extension);
    fs::rename(draft, &target).ok().map(|_| target)
}

/// A few frames spread over `video`, as JPGs in `dir`. Fewer, or none, when ffprobe
/// or ffmpeg can't read it.
fn reference_frames(ffmpeg: &Path, video: &Path, dir: &Path, env: &[(String, String)]) -> Vec<PathBuf> {
    let Some(duration) = probe_media(video).map(|media| media.duration_secs).filter(|secs| *secs > 0.0) else {
        return Vec::new();
    };
    (1..=REFERENCE_FRAMES)
        .filter_map(|n| {
            let at = format!("{:.2}", duration * n as f64 / (REFERENCE_FRAMES + 1) as f64);
            let frame = dir.join(format!("frame-{n}.jpg"));
            let args = [OsStr::new("-y"), OsStr::new("-loglevel"), OsStr::new("error"), OsStr::new("-ss")]
                .into_iter()
                .chain([OsStr::new(&at), OsStr::new("-i"), video.as_os_str()])
                .chain([OsStr::new("-frames:v"), OsStr::new("1"), OsStr::new("-vf"), OsStr::new("scale=1280:-2")])
                .chain([OsStr::new("-q:v"), OsStr::new("3"), frame.as_os_str()]);
            run_captured(ffmpeg, args, env, FFMPEG_TIMEOUT).filter(|_| frame.is_file()).map(|_| frame)
        })
        .collect()
}

/// The prompt that starts a chat: the video, what makes a thumbnail work, what the
/// creator says, and where to save what Codex draws, in order of `drafts`.
pub fn thumbnail_prompt(request: &ThumbnailRequest, drafts: &[PathBuf], frames: bool) -> String {
    let kind = if request.short { "a YouTube Short" } else { "a YouTube video" };
    let title = request.video_title.trim();
    let about = if title.is_empty() { String::new() } else { format!(" titled “{title}”") };
    let mut prompt = format!(
        "You draw thumbnails for {kind}{about} with your image generation tool, in a chat with \
         its creator inside the Rushes app.\n\n\
         You run headless: nobody can answer questions while you work, so make the most sensible \
         choices yourself. The creator reads your last message and may come back in this chat to \
         ask for changes, more ideas or your opinion, so remember what you drew.\n\n\
         What makes a thumbnail work:\n\
         - Landscape 16:9, made to read at phone size: one clear subject, big and close, strong \
         contrast and saturated color.\n\
         - Text is optional. If there is any: at most four words, huge and bold, in the language \
         of the video.\n\
         - No YouTube logos, play buttons, borders, watermarks or made-up interface.\n\
         - When you draw several, make each a different idea: another composition, emotion, text \
         or palette.\n"
    );
    if frames {
        prompt.push_str(
            "- The attached images are frames from the video. Keep its people, their look and the \
             setting recognisable; never invent other people in their place.\n",
        );
    }
    let script = request.script.trim();
    if !script.is_empty() {
        let excerpt = match script.char_indices().nth(SCRIPT_MAX_CHARS) {
            Some((cut, _)) => format!("{}\n[…]", &script[..cut]),
            None => script.to_owned(),
        };
        prompt.push_str(&format!("\nWhat the video is about, from its script:\n<script>\n{excerpt}\n</script>\n"));
    }
    prompt.push('\n');
    push_message(&mut prompt, &request.message);
    push_drawing_rules(&mut prompt, drafts, true);
    prompt
}

/// The prompt of a later turn: what the creator says, the thumbnails there are to
/// talk about (`thumbnails`, as [`list_thumbnails`] gives them) and where to save
/// what Codex draws.
pub fn follow_up_prompt(request: &ThumbnailRequest, thumbnails: &[PathBuf], drafts: &[PathBuf]) -> String {
    let mut prompt = String::new();
    push_message(&mut prompt, &request.message);
    let mut names: Vec<String> = thumbnails.iter().rev().map(|path| file_name(path)).collect();
    if !names.is_empty() {
        if names.len() > LISTED_THUMBNAILS {
            names = names.split_off(names.len() - LISTED_THUMBNAILS);
            names.insert(0, "…".to_owned());
        }
        prompt.push_str(&format!(
            "\nThe thumbnails so far are in your working folder: {}. “Number 6” or “#6” means \
             thumbnail-6.jpg. To change one, look at it and draw a new version; never edit or \
             overwrite the files there.\n",
            names.join(", "),
        ));
    }
    let last: Vec<String> = request.last_drawn.iter().map(|path| file_name(path)).collect();
    if !last.is_empty() {
        prompt.push_str(&format!(
            "Rushes saved what you drew in your last turn as {}, in the order you drew them.\n",
            last.join(", ")
        ));
    }
    push_drawing_rules(&mut prompt, drafts, false);
    prompt
}

fn push_message(prompt: &mut String, message: &str) {
    match message.trim() {
        "" => prompt.push_str("The creator hasn’t said what they want: draw a few different ideas.\n"),
        message => prompt.push_str(&format!("The creator says:\n<message>\n{message}\n</message>\n")),
    }
}

/// How many to draw, where to save them and how to answer.
fn push_drawing_rules(prompt: &mut String, drafts: &[PathBuf], first: bool) {
    let default = if first {
        format!("{DEFAULT_IDEAS} different ideas")
    } else {
        "one picture for each change they ask for".to_owned()
    };
    let paths: Vec<String> = drafts.iter().map(|path| format!("- {}", path.display())).collect();
    prompt.push_str(&format!(
        "\nDraw as many thumbnails as the creator asks for, at most {max}; when they don’t say how \
         many, draw {default}. If they only ask a question or want your opinion, answer it and draw \
         nothing.\n\n\
         Save each image as soon as it is drawn: copy the file your image tool saved to the next \
         of these paths, in order:\n{paths}\n\
         Don’t resize or convert them, Rushes does that, and don’t write anywhere else. End with a \
         short message to the creator: one line per thumbnail you drew saying its idea, or your \
         answer.",
        max = drafts.len(),
        paths = paths.join("\n"),
    ));
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|arg| arg.into_string().expect("utf-8")).collect()
    }

    #[test]
    fn codex_runs_headless_and_writes_only_where_it_works() {
        let config = CodexConfig::default();
        let frames = [PathBuf::from("/tmp/frame-1.jpg")];
        let args = strings(config.args(Path::new("/media/sourdough/thumbnails"), &frames, None));
        assert_eq!(
            args,
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox=workspace-write",
                "--cd=/media/sourdough/thumbnails",
                "--config=model_reasoning_effort=\"medium\"",
                "--image=/tmp/frame-1.jpg",
            ]
        );
        let config = CodexConfig { model: Some("gpt-image".into()), reasoning_effort: None, ..config };
        let args = strings(config.args(Path::new("/m"), &[], None));
        assert_eq!(args[4..], ["--cd=/m", "--model=gpt-image"]);
    }

    #[test]
    fn a_later_turn_resumes_the_session_with_the_same_sandbox() {
        let args = strings(CodexConfig::default().args(Path::new("/m"), &[], Some("0199-abc")));
        assert_eq!(
            args,
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox=workspace-write",
                "--cd=/m",
                "--config=model_reasoning_effort=\"medium\"",
                "resume",
                "--",
                "0199-abc",
                "-",
            ]
        );
    }

    #[test]
    fn debug_output_hides_env_values() {
        let config = CodexConfig { env: vec![("OPENAI_API_KEY".into(), "sk-secret".into())], ..CodexConfig::default() };
        let debug = format!("{config:?}");
        assert!(debug.contains("OPENAI_API_KEY") && !debug.contains("sk-secret"), "{debug}");
    }

    #[test]
    fn the_first_prompt_has_the_video_the_creators_words_and_every_draft() {
        let request = ThumbnailRequest {
            video_title: "Sourdough in 24 hours".into(),
            script: "## Starter\nFeed it.".into(),
            message: "Me holding the loaf.".into(),
            ..ThumbnailRequest::default()
        };
        let drafts = [PathBuf::from("/t/.drafts/draft-1.png"), PathBuf::from("/t/.drafts/draft-2.png")];
        let prompt = thumbnail_prompt(&request, &drafts, true);
        for part in [
            "You draw thumbnails for a YouTube video titled “Sourdough in 24 hours”",
            "may come back in this chat",
            "a different idea",
            "frames from the video",
            "<message>\nMe holding the loaf.\n</message>",
            "<script>\n## Starter\nFeed it.\n</script>",
            "at most 2; when they don’t say how many, draw 3 different ideas",
            "- /t/.drafts/draft-1.png\n- /t/.drafts/draft-2.png",
        ] {
            assert!(prompt.contains(part), "missing {part:?} in:\n{prompt}");
        }
        let empty = thumbnail_prompt(&ThumbnailRequest::default(), &drafts, false);
        assert!(empty.starts_with("You draw thumbnails for a YouTube video with"), "{empty}");
        assert!(empty.contains("hasn’t said what they want: draw a few different ideas"), "{empty}");
        assert!(!empty.contains("frames") && !empty.contains("<script>"), "{empty}");
    }

    #[test]
    fn a_later_prompt_names_the_thumbnails_to_talk_about() {
        let request = ThumbnailRequest {
            message: "Make number 2 brighter".into(),
            last_drawn: vec![PathBuf::from("/t/thumbnail-2.jpg"), PathBuf::from("/t/thumbnail-3.jpg")],
            // A later turn has seen the video already.
            script: "## Starter".into(),
            ..ThumbnailRequest::default()
        };
        let listed: Vec<PathBuf> = (1..=3).rev().map(|n| PathBuf::from(format!("/t/thumbnail-{n}.jpg"))).collect();
        let prompt = follow_up_prompt(&request, &listed, &[PathBuf::from("/t/.drafts/draft-1.png")]);
        for part in [
            "<message>\nMake number 2 brighter\n</message>",
            "in your working folder: thumbnail-1.jpg, thumbnail-2.jpg, thumbnail-3.jpg.",
            "your last turn as thumbnail-2.jpg, thumbnail-3.jpg, in the order",
            "draw one picture for each change",
            "answer it and draw nothing",
            "- /t/.drafts/draft-1.png",
        ] {
            assert!(prompt.contains(part), "missing {part:?} in:\n{prompt}");
        }
        assert!(!prompt.contains("<script>"), "{prompt}");
        let many: Vec<PathBuf> = (1..=20).rev().map(|n| PathBuf::from(format!("/t/thumbnail-{n}.jpg"))).collect();
        let prompt = follow_up_prompt(&ThumbnailRequest::default(), &many, &[]);
        assert!(prompt.contains("folder: …, thumbnail-9.jpg, thumbnail-10.jpg,"), "{prompt}");
        assert!(prompt.contains("thumbnail-20.jpg."), "{prompt}");
    }

    #[test]
    fn thumbnails_are_listed_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "thumbnail-2.jpg",
            "thumbnail-10.png",
            "thumbnail-1.jpg",
            "thumbnail-x.jpg",
            "thumbnail-3.txt",
            "cover.jpg",
        ] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        let names: Vec<String> = list_thumbnails(dir.path())
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["thumbnail-10.png", "thumbnail-2.jpg", "thumbnail-1.jpg"]);
        assert!(list_thumbnails(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn a_draft_without_ffmpeg_is_kept_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("thumbnail-4.jpg"), b"x").unwrap();
        let draft = dir.path().join("draft-1.png");
        fs::write(&draft, b"png").unwrap();
        assert_eq!(keep_draft(&draft, dir.path(), None, &[]), Some(dir.path().join("thumbnail-5.png")));
        assert!(!draft.exists());
        let empty = dir.path().join("draft-2.png");
        fs::write(&empty, b"").unwrap();
        assert_eq!(keep_draft(&empty, dir.path(), None, &[]), None, "an empty file is no draft");
        assert_eq!(keep_draft(&dir.path().join("draft-3.png"), dir.path(), None, &[]), None);
    }
}
