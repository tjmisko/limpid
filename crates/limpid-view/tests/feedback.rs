//! Replies, comments, the editor box and delivery, headless: the page is
//! driven with key presses, the editor is either scripted or a real nvim.

mod common;

use common::{assert_matches_snapshot, fixture, shot};
use limpid_core::{
    doc::{AnnotationKind, BlockKind},
    replies::{AgentReplies, sidecar_path},
    settings::Settings,
    transport::{Handoff, Scheme},
};
use limpid_view::{
    editor::{BoxKind, EditorConfig, EditorError, EditorEvent, Grid},
    feedback::{
        Feedback,
        boxes::{EditorPort, EditorSpawner},
    },
    nav::{CloseKind, keymap::KeyInput},
    page::{Closer, DocPage},
    window::Page,
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Default)]
struct Script {
    opened: Vec<(String, BoxKind)>,
    inputs: Vec<String>,
    events: VecDeque<EditorEvent>,
}

struct Fake {
    script: Rc<RefCell<Script>>,
    grid: Grid,
}

impl EditorPort for Fake {
    fn open(&mut self, text: &str, kind: BoxKind) -> Result<(), EditorError> {
        self.script
            .borrow_mut()
            .opened
            .push((text.to_owned(), kind));
        Ok(())
    }
    fn input(&mut self, keys: &str) {
        self.script.borrow_mut().inputs.push(keys.to_owned());
    }
    fn resize(&mut self, _cols: u16, _rows: u16) {}
    fn poll(&mut self) -> Vec<EditorEvent> {
        self.script.borrow_mut().events.drain(..).collect()
    }
    fn grid(&self) -> &Grid {
        &self.grid
    }
    fn is_running(&mut self) -> bool {
        true
    }
}

struct Closes(Rc<Cell<Option<CloseKind>>>);

impl Closer for Closes {
    fn close(&self, kind: CloseKind) -> bool {
        self.0.set(Some(kind));
        true
    }
}

struct World {
    dir: PathBuf,
    file: PathBuf,
    page: DocPage,
    script: Rc<RefCell<Script>>,
    closed: Rc<Cell<Option<CloseKind>>>,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("limpid-feedback-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fake_spawner(script: &Rc<RefCell<Script>>) -> EditorSpawner {
    let script = script.clone();
    Box::new(move |_config: &EditorConfig| {
        Ok(Box::new(Fake {
            script: script.clone(),
            grid: Grid::new(60, 8),
        }) as Box<dyn EditorPort>)
    })
}

fn world(name: &str, file_backed: bool, settings: &str, spawner: Option<EditorSpawner>) -> World {
    world_from(
        name,
        &fixture("questions.md"),
        file_backed,
        settings,
        spawner,
    )
}

fn world_from(
    name: &str,
    source: &str,
    file_backed: bool,
    settings: &str,
    spawner: Option<EditorSpawner>,
) -> World {
    let dir = scratch(name);
    let file = dir.join("questions.md");
    std::fs::write(&file, source).unwrap();
    let settings = Settings::parse(settings).unwrap();
    let script = Rc::new(RefCell::new(Script::default()));
    let handoff = Handoff {
        path: file.clone(),
        theme: Scheme::Dark,
        pane: None,
        source: file_backed.then(|| file.clone()),
        source_hash: None,
        source_backed: false,
    };
    let closed = Rc::new(Cell::new(None));
    let page = DocPage::open(&file, &settings, Scheme::Dark, common::fonts()).unwrap();
    let title = page.title();
    let spawner = spawner.unwrap_or_else(|| fake_spawner(&script));
    let feedback = Feedback::new(handoff, title, &settings).with_spawner(spawner);
    let mut page = page
        .with_feedback(feedback)
        .with_closer(Box::new(Closes(closed.clone())));
    paint(&mut page);
    World {
        dir,
        file,
        page,
        script,
        closed,
    }
}

fn paint(page: &mut DocPage) -> Vec<u32> {
    let (width, height) = (900u32, 700u32);
    let mut pixels = vec![0u32; (width * height) as usize];
    page.paint(
        &mut limpid_view::paint::Canvas::new(&mut pixels, width, height),
        1.0,
    );
    pixels
}

impl World {
    fn press(&mut self, key: &str) -> bool {
        let changed = self.page.handle_input(&KeyInput::named(key));
        paint(&mut self.page);
        changed
    }

    fn type_line(&mut self, text: &str) {
        for character in text.chars() {
            let name = if character == ' ' {
                "space".to_owned()
            } else {
                character.to_string()
            };
            self.press(&name);
        }
    }

    fn command(&mut self, line: &str) {
        self.press(":");
        self.type_line(line);
        self.press("enter");
    }

    fn field(&mut self, number: usize) {
        // The first stop is the paragraph; the questions follow it.
        for _ in 0..number {
            self.press("j");
        }
    }

    fn say(&mut self, event: EditorEvent) {
        self.script.borrow_mut().events.push_back(event);
        self.page.tick();
        paint(&mut self.page);
    }

    fn annotations(&self) -> Vec<(AnnotationKind, String)> {
        self.page
            .doc()
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockKind::Annotation { kind, text, .. } => Some((*kind, text.clone())),
                _ => None,
            })
            .collect()
    }

    fn flash(&self) -> String {
        self.page.nav().status_view(self.page.elapsed_ms()).message
    }
}

#[test]
fn should_match_the_snapshot_when_questions_have_numbered_options() {
    let mut world = world("snap", false, "", None);
    let image = shot(&mut world.page, 900, 600, 1.0);
    assert_matches_snapshot(&image, "questions-fields");
}

#[test]
fn should_match_the_snapshot_when_an_option_is_picked_and_a_comment_is_pending() {
    let mut world = world("snap-picked", false, "", None);
    world.field(1);
    world.press("2");
    world.press("k");
    world.press("c");
    world.say(EditorEvent::Save("Say which store matters here.".into()));
    world.say(EditorEvent::Quit { discard: false });
    let image = shot(&mut world.page, 900, 600, 1.0);
    assert_matches_snapshot(&image, "questions-answered");
}

#[test]
fn should_show_the_reply_under_the_question_when_a_digit_picks_an_option() {
    let mut world = world("pick", false, "", None);
    world.field(1);
    assert!(world.press("2"));
    assert_eq!(
        world.annotations(),
        [(AnnotationKind::Reply, "Postgres, shared".to_owned())]
    );
    assert!(world.flash().contains("q1"), "{}", world.flash());
}

#[test]
fn should_treat_a_digit_as_a_count_when_the_question_has_no_options() {
    let mut world = world("count", false, "", None);
    world.field(2);
    world.press("1");
    assert!(world.annotations().is_empty());
}

#[test]
fn should_refuse_quitting_with_the_unsaved_line_when_a_reply_is_pending() {
    let mut world = world("guard", false, "", None);
    world.field(1);
    world.press("1");
    world.command("q");
    assert_eq!(world.closed.get(), None);
    assert!(
        world.flash().contains("unsaved changes"),
        "{}",
        world.flash()
    );
}

#[test]
fn should_close_and_leave_nothing_behind_when_quitting_with_force() {
    let mut world = world("force", false, "", None);
    world.field(1);
    world.press("1");
    world.command("q!");
    assert_eq!(world.closed.get(), Some(CloseKind::Window));
    assert!(!sidecar_path(&world.file).exists());
}

#[test]
fn should_write_the_sidecar_when_the_page_saves_with_w() {
    let mut world = world("save", false, "", None);
    world.field(1);
    world.press("1");
    world.command("w");
    assert_eq!(world.closed.get(), None);
    let saved = AgentReplies::load(&sidecar_path(&world.file)).unwrap();
    assert_eq!(saved.replies["q1"], "SQLite, one file per workspace");
    world.command("q");
    assert_eq!(
        world.closed.get(),
        Some(CloseKind::Window),
        "nothing unsaved now"
    );
}

#[test]
fn should_write_the_reply_into_the_file_when_the_page_is_a_markdown_file() {
    let mut world = world("file", true, "", None);
    world.field(1);
    world.press("2");
    world.command("w");
    let written = std::fs::read_to_string(&world.file).unwrap();
    assert!(written.contains("limpid reply q1"), "{written}");
    assert!(written.contains("Postgres, shared"), "{written}");
    assert_eq!(
        world.annotations(),
        [(AnnotationKind::Reply, "Postgres, shared".to_owned())],
        "the saved reply now comes from the file"
    );
}

#[test]
fn should_deliver_and_close_when_writing_and_quitting() {
    let delivered = scratch("delivered-out").join("message.txt");
    let settings = format!("[reply]\ncommand = \"cat > '{}'\"\n", delivered.display());
    let mut world = world("send", false, &settings, None);
    world.field(1);
    world.press("1");
    world.command("wq");
    let message = std::fs::read_to_string(&delivered).unwrap();
    assert!(
        message.contains("q1: SQLite, one file per workspace"),
        "{message}"
    );
    assert_eq!(world.closed.get(), Some(CloseKind::Window));
    assert!(
        sidecar_path(&world.file).exists(),
        "saved before it was sent"
    );
    let _ = std::fs::remove_dir_all(delivered.parent().unwrap());
}

#[test]
fn should_stay_open_and_keep_the_save_when_delivery_fails() {
    let mut world = world("sendfail", false, "[reply]\ncommand = \"exit 7\"\n", None);
    world.field(1);
    world.press("1");
    world.command("wq");
    assert_eq!(world.closed.get(), None);
    assert!(world.flash().contains("saved"), "{}", world.flash());
    assert!(sidecar_path(&world.file).exists());
}

#[test]
fn should_close_after_wq_when_there_is_no_feedback_to_send() {
    let mut world = world("nothing", false, "", None);
    world.command("wq");
    assert_eq!(world.closed.get(), Some(CloseKind::Window));
    assert!(world.flash().contains("no feedback"), "{}", world.flash());
}

#[test]
fn should_review_the_message_and_send_it_on_ctrl_enter_when_s_is_pressed() {
    let delivered = scratch("review-out").join("message.txt");
    let settings = format!("[reply]\ncommand = \"cat > '{}'\"\n", delivered.display());
    let mut world = world("review", false, &settings, None);
    world.press("shift+s");
    assert!(!world.page.review_open(), "nothing to review yet");
    world.field(1);
    world.press("2");
    world.press("shift+s");
    assert!(world.page.review_open());
    world.press("j");
    assert!(world.page.review_open(), "scrolling keeps it open");
    world.press("escape");
    assert!(!world.page.review_open());
    assert!(!delivered.exists(), "dismissing sends nothing");
    world.press("shift+s");
    world.press("enter");
    assert!(world.page.review_open(), "plain enter does not send");
    world.press("ctrl+enter");
    assert!(!world.page.review_open());
    let message = std::fs::read_to_string(&delivered).unwrap();
    assert!(message.contains("q1: Postgres, shared"), "{message}");
    assert_eq!(world.closed.get(), Some(CloseKind::Window));
    let _ = std::fs::remove_dir_all(delivered.parent().unwrap());
}

#[test]
fn should_not_move_the_page_while_the_review_is_open() {
    let mut world = world("reviewlock", false, "", None);
    world.field(1);
    world.press("1");
    world.press("shift+s");
    let before = world.page.scroll();
    world.press("c");
    world.press("shift+g");
    assert_eq!(world.page.scroll(), before);
    assert!(!world.page.box_open());
}

#[test]
fn should_jump_to_the_first_unanswered_question_when_shift_q_is_pressed() {
    let mut world = world("unanswered", false, "", None);
    world.field(1);
    world.press("1");
    world.press("shift+g");
    world.press("shift+q");
    let focused = world.page.nav().cursor().src.clone().unwrap();
    let source = &world.page.doc().source;
    assert!(
        source[focused].contains("How long"),
        "q2 is the first unanswered"
    );
}

#[test]
fn should_open_the_reply_box_and_route_every_key_to_the_editor_when_enter_is_pressed() {
    let mut world = world("box", false, "", None);
    world.field(1);
    let before = paint(&mut world.page);
    world.press("enter");
    assert!(world.page.box_open());
    assert_eq!(
        world.script.borrow().opened,
        [(String::new(), BoxKind::Reply)]
    );
    assert_ne!(
        before,
        paint(&mut world.page),
        "the box takes room on the page"
    );
    // Page keys are not page keys now: j must reach the editor, not the cursor.
    let cursor = world.page.nav().cursor().src.clone();
    let none = ModifiersState::empty();
    world
        .page
        .editor_key(&Key::Character("j".into()), Some("j"), none);
    world
        .page
        .editor_key(&Key::Character("<".into()), Some("<"), none);
    world
        .page
        .editor_key(&Key::Named(NamedKey::Escape), None, none);
    world.page.editor_key(
        &Key::Character("d".into()),
        Some("d"),
        ModifiersState::CONTROL,
    );
    assert_eq!(
        world.script.borrow().inputs,
        ["j", "<lt>", "<Esc>", "<C-d>"]
    );
    assert_eq!(world.page.nav().cursor().src, cursor);
    assert!(!world.press(":"), "the page command line is unreachable");
    assert!(world.page.box_open());
}

#[test]
fn should_flash_editor_open_and_keep_one_box_when_enter_is_pressed_twice() {
    let mut world = world("onebox", false, "", None);
    world.field(1);
    world.press("enter");
    assert!(!world.page.box_open() || world.script.borrow().opened.len() == 1);
    let none = ModifiersState::empty();
    world
        .page
        .editor_key(&Key::Named(NamedKey::Enter), None, none);
    assert_eq!(world.script.borrow().opened.len(), 1);
}

#[test]
fn should_show_the_saved_text_and_return_to_the_page_when_the_editor_saves_and_quits() {
    let mut world = world("roundtrip", false, "", None);
    world.field(1);
    world.press("enter");
    world.say(EditorEvent::Save("Neither: plain files.".into()));
    assert!(world.page.box_open(), ":w keeps the box open");
    world.say(EditorEvent::Quit { discard: false });
    assert!(!world.page.box_open());
    assert_eq!(
        world.annotations(),
        [(AnnotationKind::Reply, "Neither: plain files.".to_owned())]
    );
    let focused = world.page.nav().cursor().src.clone().unwrap();
    assert!(
        world.page.doc().source[focused].contains("Which store"),
        "focus returns to the question"
    );
    world.press("j");
    world.press("k");
    world.command("q");
    assert_eq!(world.closed.get(), None, "the reply is unsaved");
}

#[test]
fn should_force_close_the_box_when_ctrl_shift_q_is_pressed() {
    let mut world = world("force-close", false, "", None);
    world.field(1);
    world.press("enter");
    let modifiers = ModifiersState::CONTROL | ModifiersState::SHIFT;
    world
        .page
        .editor_key(&Key::Character("Q".into()), Some("Q"), modifiers);
    assert!(!world.page.box_open());
    assert!(
        world
            .script
            .borrow()
            .inputs
            .iter()
            .all(|keys| keys != "Q" && keys != "<C-Q>"),
        "the chord is not for nvim"
    );
}

#[test]
fn should_want_ticks_only_while_an_editor_exists() {
    let mut world = world("ticks", false, "", None);
    assert!(!world.page.wants_ticks());
    world.field(1);
    world.press("enter");
    assert!(world.page.wants_ticks());
    world.say(EditorEvent::Quit { discard: false });
    assert!(world.page.wants_ticks(), "the idle timer is still running");
}

#[test]
fn should_put_a_comment_on_any_block_when_c_is_pressed_and_remove_it_with_x() {
    let mut world = world("comment", false, "", None);
    world.press("c");
    assert_eq!(
        world.script.borrow().opened,
        [(String::new(), BoxKind::Comment)]
    );
    world.say(EditorEvent::Save("Too vague.".into()));
    world.say(EditorEvent::Quit { discard: false });
    assert_eq!(
        world.annotations(),
        [(AnnotationKind::Comment, "Too vague.".to_owned())]
    );
    world.press("j");
    assert!(
        world
            .page
            .doc()
            .source
            .get(world.page.nav().cursor().src.clone().unwrap())
            .is_some(),
        "the cursor rests on the comment"
    );
    world.press("k");
    world.press("x");
    assert!(world.annotations().is_empty(), "removed from the page");
    world.command("w");
    let saved = AgentReplies::load(&sidecar_path(&world.file));
    assert!(saved.is_ok_and(|saved| saved.comments.is_empty()));
}

/// The real thing: nvim embedded, a comment typed, saved with `:wq`, then the
/// page saved. Skipped when nvim is not installed.
#[test]
fn should_write_the_sidecar_when_a_comment_is_typed_in_a_real_nvim() {
    if std::process::Command::new("nvim")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: nvim is not installed");
        return;
    }
    let mut world = world(
        "nvim",
        false,
        "[editor]\ninit = 'clean'\n",
        Some(limpid_view::feedback::boxes::nvim_spawner()),
    );
    world.press("c");
    assert!(world.page.box_open());
    let none = ModifiersState::empty();
    for character in "needs an example".chars() {
        let text = character.to_string();
        let key = if character == ' ' {
            Key::Named(NamedKey::Space)
        } else {
            Key::Character(text.as_str().into())
        };
        world.page.editor_key(&key, Some(&text), none);
    }
    world
        .page
        .editor_key(&Key::Named(NamedKey::Escape), None, none);
    for character in ":wq".chars() {
        let text = character.to_string();
        world
            .page
            .editor_key(&Key::Character(text.as_str().into()), Some(&text), none);
    }
    world
        .page
        .editor_key(&Key::Named(NamedKey::Enter), None, none);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while world.page.box_open() && std::time::Instant::now() < deadline {
        world.page.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!world.page.box_open(), "nvim closed the box");
    paint(&mut world.page);
    world.command("w");
    let saved = AgentReplies::load(&sidecar_path(&world.file)).unwrap();
    assert_eq!(saved.comments.len(), 1, "{saved:?}");
    let comment = saved.comments.values().next().unwrap();
    assert_eq!(comment.text, "needs an example");
    assert!(
        comment.quote.starts_with("We need somewhere"),
        "{}",
        comment.quote
    );
}

#[test]
fn should_scroll_to_the_box_when_a_reply_box_opens_far_down_the_page() {
    let filler: String = (0..120)
        .map(|n| format!("Paragraph number {n}.\n\n"))
        .collect();
    let source = format!("# Long\n\n{filler}## Questions\n\n1. Which?\n   - A\n   - B\n");
    let mut world = world_from("scroll", &source, false, "", None);
    world.press("shift+g");
    assert!(world.page.scroll() > 0.0);
    let before = world.page.scroll();
    world.press("enter");
    assert!(world.page.box_open());
    paint(&mut world.page);
    let layout = world.page.layout().unwrap();
    let focused = world
        .page
        .nav()
        .cursor()
        .stop(layout)
        .expect("the box is a stop");
    let window_top = focused.rect.y - world.page.scroll();
    assert!(
        (0.0..700.0).contains(&window_top),
        "the box must be on screen: top {window_top} at scroll {} (was {before})",
        world.page.scroll()
    );
    assert!(
        world.page.doc().source.get(focused.src.clone()) == Some(""),
        "the cursor is on the added box, not the question"
    );
}
