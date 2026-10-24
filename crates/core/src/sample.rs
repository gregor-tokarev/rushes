//! Sample workspace for onboarding ("Start with sample videos") and UI demos:
//! a small tech channel with videos in every production stage.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use uuid::Uuid;

use crate::model::{
    Block, BlockKind, Document, DocumentId, DocumentKind, Privacy, PropertyDef, PropertyId, PropertyKind,
    PropertyValue, PublishSettings, SelectOption, TagColor, Video, VideoFormat, VideoStatus, YoutubeLink,
};
use crate::templates::{
    TemplateKind, broll, bullet, callout, h2, h3, onscreen, p, script_template, talking_point, todo,
};

pub(crate) struct SampleWorkspace {
    pub properties: Vec<PropertyDef>,
    /// Videos with positions `0.0, 1.0, ...` and their documents.
    pub videos: Vec<(Video, Vec<Document>)>,
}

struct Options {
    priority: PropertyDef,
    sponsor: PropertyDef,
}

impl Options {
    fn select(def: &PropertyDef, name: &str) -> PropertyValue {
        let option = def.kind.options().iter().find(|option| option.name == name);
        PropertyValue::Select(option.map(|option| option.id.clone()))
    }

    fn values(&self, priority: &str, sponsor: Option<&str>) -> BTreeMap<PropertyId, PropertyValue> {
        let mut values = BTreeMap::from([(self.priority.id, Self::select(&self.priority, priority))]);
        if let Some(sponsor) = sponsor {
            values.insert(self.sponsor.id, Self::select(&self.sponsor, sponsor));
        }
        values
    }
}

fn select_property(name: &str, position: i32, options: &[(&str, TagColor)]) -> PropertyDef {
    let options = options
        .iter()
        .map(|&(name, color)| SelectOption { id: Uuid::now_v7().simple().to_string(), name: name.to_string(), color })
        .collect();
    PropertyDef { id: PropertyId::new(), name: name.to_string(), kind: PropertyKind::Select { options }, position }
}

pub(crate) fn sample_workspace() -> SampleWorkspace {
    let options = Options {
        priority: select_property(
            "Priority",
            0,
            &[("High", TagColor::Red), ("Medium", TagColor::Yellow), ("Low", TagColor::Gray)],
        ),
        sponsor: select_property(
            "Sponsor",
            1,
            &[("Glyph Keyboards", TagColor::Purple), ("CloudNest", TagColor::Blue), ("Skillforge", TagColor::Green)],
        ),
    };
    let today = Utc::now().date_naive();
    let videos = vec![
        mini_pc_review(&options, today),
        rust_ownership(&options, today),
        python_short(&options, today),
        desk_setup(&options, today),
        sqlite_idea(&options),
        notion_livestream(&options, today),
        channel_trailer(&options),
    ];
    let videos = videos
        .into_iter()
        .enumerate()
        .map(|(index, (mut video, script, notes))| {
            video.position = index as f64;
            let documents = vec![
                document(&video, DocumentKind::Script, "Script", script, 0),
                document(&video, DocumentKind::Notes, "Notes", notes, 1),
            ];
            (video, documents)
        })
        .collect();
    SampleWorkspace { properties: vec![options.priority, options.sponsor], videos }
}

fn document(video: &Video, kind: DocumentKind, title: &str, blocks: Vec<Block>, position: i32) -> Document {
    let blocks = if blocks.is_empty() { vec![Block::paragraph("")] } else { blocks };
    Document {
        id: DocumentId::new(),
        video_id: video.id,
        kind,
        title: title.to_string(),
        blocks,
        position,
        updated_at: Utc::now(),
    }
}

fn video(title: &str, icon: &str, status: VideoStatus, format: VideoFormat, tags: &[&str], hook: &str) -> Video {
    Video {
        icon: Some(icon.to_string()),
        status,
        format,
        tags: tags.iter().map(|tag| tag.to_string()).collect(),
        hook: hook.to_string(),
        ..Video::new(title)
    }
}

fn days_from(today: NaiveDate, days: i64) -> Option<NaiveDate> {
    today.checked_add_signed(TimeDelta::days(days))
}

fn at_hour(date: Option<NaiveDate>, hour: u32) -> Option<DateTime<Utc>> {
    date.and_then(|date| date.and_hms_opt(hour, 0, 0)).map(|time| time.and_utc())
}

fn checked(text: &str) -> Block {
    Block { checked: true, ..todo(text) }
}

fn code(text: &str) -> Block {
    Block::new(BlockKind::Code, text)
}

type Sample = (Video, Vec<Block>, Vec<Block>);

fn mini_pc_review(options: &Options, today: NaiveDate) -> Sample {
    let mut video = video(
        "I Replaced My Laptop With a $200 Mini PC for 30 Days",
        "🖥️",
        VideoStatus::Published,
        VideoFormat::LongForm,
        &["mini pc", "linux", "review"],
        "A $200 box replaced my laptop for a month. Here's what broke first.",
    );
    video.publish_date = days_from(today, -15);
    video.properties = options.values("Medium", None);
    video.publish = PublishSettings {
        title: String::new(),
        description: "I used a $200 Intel N100 mini PC as my only computer for 30 days.\n\n\
                      0:00 The experiment\n0:42 Specs and price\n1:30 Week one\n2:55 What broke on day three\n\
                      4:10 Editing and compiling\n6:02 What I don't like\n7:15 Verdict"
            .to_string(),
        tags: vec!["mini pc".into(), "n100".into(), "linux desktop".into(), "budget pc".into()],
        category_id: "28".to_string(),
        privacy: Privacy::Public,
        ..PublishSettings::default()
    };
    video.youtube = at_hour(video.publish_date, 15).map(|uploaded_at| YoutubeLink {
        video_id: "mP4c-0n30Ds".to_string(),
        url: "https://www.youtube.com/watch?v=mP4c-0n30Ds".to_string(),
        channel_id: "UCq3v8bX2fZ0sAmPleCh4nnel".to_string(),
        privacy: Privacy::Public,
        uploaded_at,
        thumbnail: None,
        publish_at: None,
        details: None,
    });

    let script = vec![
        h2("Hook"),
        p("Thirty days ago I put my laptop in a drawer and plugged in *this*: a two-hundred-dollar mini PC that fits \
           in my palm. I edited videos on it, compiled Rust on it and joined every call on it. One thing broke on day \
           three, and it wasn't what I expected."),
        broll("Slow push-in on the mini PC next to the laptop; drawer closing."),
        onscreen("$200 vs $2,000"),
        h2("Context"),
        p("It has an Intel N100, 16 gigs of RAM and a 512 gig SSD, and it costs about as much as a nice keyboard. I \
           bought it with my own money; nobody sponsored this video."),
        onscreen("Intel N100 · 16 GB RAM · 512 GB SSD · ~$200"),
        h2("Week one: the honeymoon"),
        p("Setup took twelve minutes. I installed Fedora, signed into everything and honestly forgot I wasn't on my \
           main machine. Browsing, email, docs, calls: all instant."),
        broll("Screen recording: boot-to-desktop timer."),
        h2("Day three: what broke"),
        p("Then I plugged in my second monitor. The HDMI port only does 4K at **30 hertz**, so my mouse felt like it \
           was moving through syrup. A twelve-dollar USB-C to DisplayPort adapter fixed it, but nobody mentions that \
           in reviews."),
        onscreen("Tip: USB-C → DisplayPort for 4K at 60 Hz"),
        h2("Real work: editing and compiling"),
        p(
            "Editing 1080p was usable with proxies and painful without. A clean build of my Rust side project took four \
           minutes eleven seconds, versus fifty-eight seconds on the laptop.",
        ),
        broll("Side-by-side compile timers."),
        onscreen("Clean Rust build: 4:11 vs 0:58"),
        h2("What I don't like"),
        bullet("4K at 30 Hz over HDMI out of the box"),
        bullet("The fan spins up during exports, and you hear it on a quiet desk"),
        bullet("Only one USB-C port"),
        h2("Who should buy it"),
        p("If your day is browser tabs, documents and calls, this is all the computer you need. If you edit 4K or \
           compile big projects, keep your laptop and use this as a second desk machine."),
        h2("Verdict"),
        p("After thirty days I'm keeping it: not as my only computer, but as the machine that lives on my desk. The \
           exact model and the adapter are linked in the description, and if you want to see it handle a full 4K \
           edit, watch this next."),
        onscreen("Verdict: 8/10 for $200"),
        Block::new(BlockKind::Divider, ""),
        h3("Production checklist"),
        checked("Record A-roll"),
        checked("Capture compile and export timers"),
        checked("Shoot product beauty shots"),
        checked("Design the thumbnail"),
        checked("Write title, description and chapters"),
    ];
    let notes = vec![
        h2("After publishing"),
        bullet("Top comment asks about running Proxmox on it: strong follow-up idea"),
        bullet("Retention dips at 2:40; the spec section runs long"),
        todo("Pin a comment with the adapter link"),
    ];
    (video, script, notes)
}

fn rust_ownership(options: &Options, today: NaiveDate) -> Sample {
    let mut video = video(
        "Rust Ownership Finally Explained in 10 Minutes",
        "🦀",
        VideoStatus::Editing,
        VideoFormat::LongForm,
        &["rust", "programming", "tutorial"],
        "Predict what the borrow checker will say before you hit save.",
    );
    video.publish_date = days_from(today, 5);
    video.properties = options.values("High", Some("CloudNest"));
    video.publish.category_id = "27".to_string();

    let script = vec![
        h2("Hook · 0:00–0:20"),
        p(
            "If the borrow checker has ever yelled at you and you just added `.clone()` until it stopped, this video is \
           for you. In ten minutes you'll be able to predict what the compiler is going to say before you hit save.",
        ),
        broll("Compiler errors scrolling past, then a clean `cargo build`."),
        onscreen("Rust Ownership Finally Explained"),
        h2("Intro · who this is for"),
        p("You know another language, you've written a little Rust, and ownership still feels like magic. You need a \
           terminal and `cargo`, that's it."),
        h2("Step 1 · every value has one owner"),
        p("Think of a value like a house key. Exactly one variable holds it. When you write `let b = a;` for a \
           `String`, you hand the key over, and `a` doesn't have it anymore."),
        code("let a = String::from(\"hello\");\nlet b = a;\nprintln!(\"{a}\"); // error: borrow of moved value"),
        broll("Animation: the key moves from `a` to `b`."),
        onscreen("Rule 1: one owner"),
        h2("Step 2 · borrowing is lending"),
        p("Most of the time you don't want to give the key away, you want to lend it. `&a` lends it for reading, to \
           as many people as you like. `&mut a` lends it for writing, to **one** person at a time."),
        onscreen("Many `&T` or one `&mut T`"),
        h2("Step 3 · lifetimes are the length of the loan"),
        p("A reference can't outlive the thing it points to. That's all a lifetime is. When the compiler asks for \
           one, it's asking which owner the loan comes from."),
        broll("Screen recording: fixing the dangling-reference example."),
        h2("Common mistakes"),
        bullet("Cloning to silence the compiler: fine for prototypes, slow in hot loops"),
        bullet("Returning a reference to a local variable"),
        bullet("Holding a `&mut` across a call that needs the same data"),
        callout("Sponsor read (CloudNest): 45 seconds after the mistakes section, before the recap."),
        h2("Recap & call to action"),
        p("One owner; many readers or one writer; loans can't outlive the owner. That's the whole model. Next, watch \
           the smart pointers video, where `Rc` and `RefCell` bend these rules on purpose."),
        onscreen("End screen: Smart pointers explained"),
        Block::new(BlockKind::Divider, ""),
        h3("Production checklist"),
        checked("Record A-roll"),
        checked("Capture screen recordings for every step"),
        todo("Animate the key-passing diagram"),
        todo("Design the thumbnail"),
        todo("Write title, description and chapters"),
    ];
    let notes = vec![
        h2("Research"),
        bullet("[The Rust Book, chapter 4](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html)"),
        bullet("Most common question in comments: why can't I have two `&mut`?"),
    ];
    (video, script, notes)
}

fn python_short(options: &Options, today: NaiveDate) -> Sample {
    let mut video = video(
        "Stop Using print() to Debug Python",
        "🐍",
        VideoStatus::Scripting,
        VideoFormat::Short,
        &["python", "shorts", "debugging"],
        "Stop debugging Python with print.",
    );
    video.publish_date = days_from(today, 3);
    video.properties = options.values("Medium", None);

    let script = vec![
        h2("Hook · 0–3s"),
        p("Stop debugging Python with print."),
        onscreen("Stop using print() to debug"),
        h2("Payoff · 3–45s"),
        p(
            "Type `breakpoint()` on the line you care about and run your script. You're now *inside* the program: print \
           any variable, step to the next line with `n`, continue with `c`.",
        ),
        broll("Terminal drops into pdb; typing `n` and `c`."),
        p("Still want a quick print? Put an equals sign in an f-string, and it prints the name and the value."),
        onscreen("f\"{total=}\" → total=42"),
        h2("Loop · 45–55s"),
        p("So next time you reach for print, remember: stop debugging Python with print."),
        Block::new(BlockKind::Divider, ""),
        h3("Production checklist"),
        todo("Frame vertically (9:16) inside the safe area"),
        todo("Burn in captions"),
        todo("Write title and description"),
    ];
    (video, script, Vec::new())
}

fn desk_setup(options: &Options, today: NaiveDate) -> Sample {
    let mut video = video(
        "My 2026 Desk Setup: Everything That Survived",
        "🎥",
        VideoStatus::Filming,
        VideoFormat::LongForm,
        &["desk setup", "productivity"],
        "If I haven't touched it in a month, it's gone. This is what survived.",
    );
    video.publish_date = days_from(today, 12);
    video.properties = options.values("High", Some("Glyph Keyboards"));

    let script = vec![
        h2("Cold open"),
        p("Everything on this desk earned its spot. If I haven't touched it in a month, it's gone. This is what \
           survived."),
        broll("Overhead timelapse of the desk being cleared and rebuilt."),
        h2("Intro"),
        p("I've changed this setup four times in two years. Today: what stayed, what I regret buying, and the \
           twenty-dollar thing that made the biggest difference."),
        onscreen("My 2026 Desk Setup"),
        h2("Beat 1 · the essentials"),
        p("A monitor arm, one good light and a keyboard I actually like typing on. Glyph Keyboards sent the \
           low-profile board you'll see in every shot; more on that in a minute."),
        broll("Close-ups: monitor arm, key light, typing sounds."),
        callout("Sponsor segment: 60–90 seconds, show the hot-swap switches, mention code DESK10."),
        h2("Beat 2 · the regrets"),
        talking_point("The ultrawide: loved it for a week, then I was turning my head like I was watching tennis"),
        talking_point("The standing desk I stood at twice; why I'd still buy it again"),
        broll("Ultrawide boxed up in the closet."),
        h2("Beat 3 · the twenty-dollar upgrade"),
        p("A cable tray under the desk. It's boring, nobody films it, and it's the reason this desk stays clean."),
        broll("Slow pan under the desk."),
        h2("Outro"),
        p("The full parts list is in the description. Tell me the one thing on your desk you'd never give up."),
        Block::new(BlockKind::Divider, ""),
        h3("Production checklist"),
        checked("Charge batteries and clear SD cards"),
        todo("Record A-roll"),
        todo("Capture the B-roll from the cues above"),
        todo("Send the sponsor segment for approval"),
        todo("Design the thumbnail"),
    ];
    let notes = vec![bullet("Shoot at golden hour for the window light"), bullet("Ask Glyph for the review deadline")];
    (video, script, notes)
}

fn sqlite_idea(options: &Options) -> Sample {
    let mut video = video(
        "Why Every Developer Should Learn SQLite",
        "💡",
        VideoStatus::Idea,
        VideoFormat::LongForm,
        &["sqlite", "databases"],
        "SQLite is the most deployed database in the world, and you're already using it.",
    );
    video.properties = options.values("Low", None);
    let script = script_template(TemplateKind::Tutorial, &video);
    (video, script, vec![p("Angle: SQLite in production, WAL mode, backups with Litestream.")])
}

fn notion_livestream(options: &Options, today: NaiveDate) -> Sample {
    let mut video = video(
        "Live: Building a Notion Clone in Rust",
        "🔴",
        VideoStatus::Scheduled,
        VideoFormat::Livestream,
        &["rust", "live coding"],
        "Three hours, one empty Cargo project, and a working block editor by the end.",
    );
    video.publish_date = days_from(today, 7);
    video.properties = options.values("Medium", Some("Skillforge"));
    video.publish.privacy = Privacy::Private;
    video.publish.publish_at = at_hour(video.publish_date, 17);
    let script = script_template(TemplateKind::Tutorial, &video);
    (video, script, Vec::new())
}

fn channel_trailer(options: &Options) -> Sample {
    let mut video = video(
        "Channel Trailer (2024)",
        "📼",
        VideoStatus::Archived,
        VideoFormat::Short,
        &["trailer"],
        "Tech that earns its place on your desk, tested for a month before I say a word.",
    );
    video.properties = options.values("Low", None);
    let script = script_template(TemplateKind::Short, &video);
    (video, script, Vec::new())
}
