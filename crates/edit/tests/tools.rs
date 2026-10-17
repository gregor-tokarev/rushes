//! The tools end to end on generated footage: a transcript in the cache (no API call),
//! a cut plan by phrase ids, renders, and the checks the agent reads.

use std::fs;
use std::path::Path;
use std::process::Command;

use rushes_edit::tools::{Content, Tools};
use serde_json::{Value, json};

fn ffmpeg_available() -> bool {
    ["ffmpeg", "ffprobe"]
        .iter()
        .all(|tool| Command::new(tool).arg("-version").output().is_ok_and(|out| out.status.success()))
}

/// 12 s of test pattern with pink noise (a signal whose position can be measured).
fn make_take(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-f", "lavfi"])
        .args([
            "-i",
            "anoisesrc=color=pink:seed=42:amplitude=0.3",
            "-t",
            "12",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .args(["-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .expect("run ffmpeg");
    assert!(status.success());
}

/// Words every 0.4 s from 0.1 to 4 s (phrase A1), a 2 s pause, then words from 6 to 11 s
/// with a filler at 7.2 s (phrase A2).
fn write_transcript(edit: &Path) {
    let mut words = Vec::new();
    let mut add = |text: &str, start: f64, end: f64| {
        words.push(json!({"text": text, "start": start, "end": end, "type": "word", "speaker_id": "s0"}));
    };
    let mut t = 0.1;
    let mut n = 0;
    while t < 3.9 {
        n += 1;
        add(&format!("one{n}"), t, t + 0.3);
        t += 0.4;
    }
    t = 6.0;
    while t < 11.0 {
        n += 1;
        if (t - 7.2).abs() < 0.01 {
            add("um,", t, t + 0.3);
        } else if (t - 10.8).abs() < 0.01 {
            add("end.", t, t + 0.3);
        } else {
            add(&format!("two{n}"), t, t + 0.3);
        }
        t += 0.4;
    }
    fs::create_dir_all(edit.join("transcripts")).unwrap();
    fs::write(edit.join("transcripts/take.json"), json!({"language_code": "eng", "words": words}).to_string()).unwrap();
}

fn text(result: &rushes_edit::tools::ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|item| match item {
            Content::Text(text) => Some(text.as_str()),
            Content::Png(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn call(tools: &mut Tools, name: &str, args: Value) -> String {
    let result = tools.call(name, args, &|_| {});
    let out = text(&result);
    assert!(!result.is_error, "{name} failed: {out}");
    out
}

#[test]
fn a_plan_renders_frame_and_sample_exact() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let footage = dir.path();
    make_take(&footage.join("take.mp4"));
    write_transcript(&footage.join("edit"));
    let mut tools = Tools::new(footage);

    let inventory = call(&mut tools, "inventory", json!({}));
    assert!(inventory.contains("A: take.mp4 (0:12.00, 320x240, 30 fps)"), "{inventory}");
    let transcript = call(&mut tools, "read_transcript", json!({}));
    assert!(transcript.contains("A1 ") && transcript.contains("A2 "), "{transcript}");
    assert!(transcript.contains("~um,~"), "fillers are marked: {transcript}");

    // The second phrase first, then a time segment of the pause, then the first phrase.
    let receipt = call(
        &mut tools,
        "plan_cut",
        json!({"segments": [
            {"from": "A2", "label": "second"},
            {"take": "A", "start": 4.3, "end": 5.6, "label": "pause"},
            {"from": "A1", "ends_with": "one9", "label": "first"},
        ]}),
    );
    assert!(receipt.contains("Cut: 3 segments"), "{receipt}");
    assert!(receipt.contains("1 fillers cut"), "{receipt}");

    for quality in ["preview", "final"] {
        let report = call(&mut tools, "render", json!({"quality": quality}));
        assert!(report.contains("No technical problems found."), "{report}");
        assert!(report.contains("median offset 0.0 ms"), "audio lands where the plan puts it: {report}");
    }
    let edl: Value = serde_json::from_slice(&fs::read(footage.join("edit/edl.json")).unwrap()).unwrap();
    let ranges = edl["ranges"].as_array().unwrap();
    assert!(ranges.len() >= 4, "the filler splits the second phrase: {edl}");
    assert!(ranges[0]["start"].as_f64().unwrap() > 5.8, "starts with the second phrase: {edl}");
    assert!(footage.join("edit/final.srt").is_file());

    // A time segment that cuts through a word is flagged; an unknown phrase is an error the agent reads.
    let receipt = call(&mut tools, "plan_cut", json!({"segments": [{"take": "A", "start": 0.2, "end": 2.0}]}));
    assert!(receipt.contains("cuts through the word"), "{receipt}");
    let error = tools.call("plan_cut", json!({"segments": [{"from": "A9"}]}), &|_| {});
    assert!(error.is_error && text(&error).contains("no phrase “A9”"), "{}", text(&error));
}

#[test]
fn vertical_output_crops_and_fits() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let footage = dir.path();
    make_take(&footage.join("take.mp4"));
    write_transcript(&footage.join("edit"));
    let mut tools = Tools::new(footage);
    call(
        &mut tools,
        "plan_cut",
        json!({"aspect": "9:16", "punch_in": true, "segments": [
            {"from": "A1"},
            {"take": "A", "start": 4.3, "end": 5.6, "framing": "fit"},
            {"from": "A2", "focus_x": 0.3},
        ]}),
    );
    let report =
        call(&mut tools, "render", json!({"quality": "preview", "burn_subtitles": true, "subtitle_style": "punchy"}));
    assert!(report.contains("No technical problems found."), "{report}");
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(footage.join("edit/preview.mp4"))
        .output()
        .unwrap();
    // As tall as the source is wide (its long side), in a 9:16 shape.
    assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "180,320");
}
