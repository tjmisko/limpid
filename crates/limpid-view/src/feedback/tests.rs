//! Behaviour of [`Feedback`] against a scripted editor, so none of it needs
//! nvim or a display.

use super::boxes::EditorPort;
use super::*;
use crate::editor::{Cell, EditorError, Grid};
use limpid_core::{
    doc::{Doc, Mode, parse},
    transport::Scheme,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

const QUESTIONS: &str = "# Plan\n\nIntro paragraph.\n\n## Questions\n\n1. Which store?\n   - SQLite\n   - Postgres\n2. Why?\n";

#[derive(Default)]
struct Script {
    spawns: usize,
    refuse_spawn: bool,
    opened: Vec<(String, BoxKind)>,
    inputs: Vec<String>,
    events: VecDeque<EditorEvent>,
    running: bool,
    resizes: Vec<(u16, u16)>,
    grid: Option<Grid>,
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

    fn resize(&mut self, cols: u16, rows: u16) {
        self.script.borrow_mut().resizes.push((cols, rows));
    }

    fn poll(&mut self) -> Vec<EditorEvent> {
        let mut script = self.script.borrow_mut();
        if let Some(grid) = script.grid.take() {
            self.grid = grid;
        }
        script.events.drain(..).collect()
    }

    fn grid(&self) -> &Grid {
        &self.grid
    }

    fn is_running(&mut self) -> bool {
        self.script.borrow().running
    }
}

struct Harness {
    dir: PathBuf,
    page: PathBuf,
    fb: Feedback,
    script: Rc<RefCell<Script>>,
    doc: Doc,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "limpid-feedback-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn harness_with(name: &str, source: &str, file_backed: bool, settings: &str) -> Harness {
    let dir = scratch(name);
    let page = dir.join("page.md");
    std::fs::write(&page, source).unwrap();
    let settings = Settings::parse(settings).unwrap();
    let handoff = Handoff {
        path: page.clone(),
        theme: Scheme::Dark,
        pane: None,
        source: file_backed.then(|| page.clone()),
        source_hash: None,
        source_backed: false,
    };
    let script = Rc::new(RefCell::new(Script {
        running: true,
        ..Script::default()
    }));
    let spawn_script = script.clone();
    let spawner: EditorSpawner = Box::new(move |_config| {
        let mut script = spawn_script.borrow_mut();
        script.spawns += 1;
        if script.refuse_spawn {
            return Err(EditorError::Spawn(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no nvim",
            )));
        }
        script.running = true;
        Ok(Box::new(Fake {
            script: spawn_script.clone(),
            grid: Grid::new(40, 6),
        }) as Box<dyn EditorPort>)
    });
    let mut fb = Feedback::new(handoff, "Plan".into(), &settings).with_spawner(spawner);
    let doc = parse(source, Mode::File, &settings);
    fb.index_document(&doc.blocks, &doc.source);
    fb.overlay(&doc.blocks);
    Harness {
        dir,
        page,
        fb,
        script,
        doc,
    }
}

fn harness(source: &str, file_backed: bool) -> Harness {
    harness_with("h", source, file_backed, "")
}

impl Harness {
    fn field_src(&self, id: &str) -> Range<usize> {
        self.fb.index.field(id).unwrap().src.clone()
    }

    fn paragraph_src(&self) -> Range<usize> {
        self.doc.blocks[1].src.clone()
    }

    fn rebuild(&mut self) {
        self.fb.overlay(&self.doc.blocks.clone());
    }

    fn say(&mut self, event: EditorEvent) {
        self.script.borrow_mut().events.push_back(event);
        self.fb.tick(Instant::now());
    }

    fn reload(&mut self) {
        let bytes = std::fs::read(&self.page).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        self.doc = parse(&text, Mode::File, &Settings::default());
        self.fb.source_reloaded(&bytes);
        self.fb.index_document(&self.doc.blocks, &self.doc.source);
        self.fb.overlay(&self.doc.blocks.clone());
    }
}

#[test]
fn should_pick_the_numbered_option_when_a_digit_is_pressed_on_a_field() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q1");
    let result = h.fb.act(FeedbackAction::PickOption(2), Some(&src));
    assert_eq!(result, ActionResult::Done("q1: Postgres".into()));
    assert_eq!(h.fb.pending().reply("q1"), Some("Postgres"));
    assert!(h.fb.has_unsaved());
    assert!(h.fb.take_stale());
}

#[test]
fn should_replace_the_choice_when_another_option_is_picked() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q1");
    h.fb.act(FeedbackAction::PickOption(1), Some(&src));
    h.fb.act(FeedbackAction::PickOption(2), Some(&src));
    assert_eq!(h.fb.pending().reply("q1"), Some("Postgres"));
}

#[test]
fn should_refuse_an_option_number_the_field_does_not_have() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q1");
    for number in [0u8, 3, 9] {
        let result = h.fb.act(FeedbackAction::PickOption(number), Some(&src));
        assert!(
            matches!(result, ActionResult::Nothing(_)),
            "{number}: {result:?}"
        );
    }
    assert_eq!(h.fb.pending().reply("q1"), None);
    assert!(!h.fb.has_unsaved());
}

#[test]
fn should_refuse_to_pick_when_the_stop_is_not_a_question() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    let result = h.fb.act(FeedbackAction::PickOption(1), Some(&src));
    assert_eq!(result, ActionResult::Nothing("no question here".into()));
    assert!(matches!(
        h.fb.act(FeedbackAction::PickOption(1), None),
        ActionResult::Nothing(_)
    ));
}

#[test]
fn should_treat_digits_as_options_only_when_the_field_has_some() {
    let h = harness(QUESTIONS, false);
    assert!(h.fb.digit_picks(&h.field_src("q1")));
    assert!(
        !h.fb.digit_picks(&h.field_src("q2")),
        "a field without options leaves digits to counts"
    );
    assert!(!h.fb.digit_picks(&h.paragraph_src()));
    assert!(!h.fb.digit_picks(&(0..0)));
}

#[test]
fn should_number_fields_through_the_actions_handle_when_asked_by_the_page() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q1");
    let placeholder = Feedback::new(h.fb.handoff().clone(), "x".into(), &Settings::default());
    let fb = std::mem::replace(&mut h.fb, placeholder);
    let mut handle = FeedbackHandle(Rc::new(RefCell::new(fb)));
    assert!(handle.digit_selects_option(&src));
    assert_eq!(
        handle.feedback(FeedbackAction::PickOption(1), Some(&src)),
        ActionResult::Done("q1: SQLite".into())
    );
    assert!(handle.has_unsaved());
    assert_eq!(
        handle.write(),
        ActionResult::Done("Saved".into()),
        "the handle forwards :w"
    );
    assert!(!handle.has_unsaved());
}

#[test]
fn should_open_a_reply_box_with_the_pending_text_when_enter_is_pressed_on_a_field() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q1");
    h.fb.act(FeedbackAction::PickOption(1), Some(&src));
    let result = h.fb.act(FeedbackAction::OpenReply, Some(&src));
    assert!(matches!(result, ActionResult::Done(_)), "{result:?}");
    assert!(h.fb.box_open());
    assert_eq!(
        h.script.borrow().opened,
        [("SQLite".to_owned(), BoxKind::Reply)]
    );
    assert_eq!(h.fb.take_focus(), Some(Focus::Box));
}

#[test]
fn should_open_an_empty_reply_box_when_the_field_has_no_answer() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q2");
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    assert_eq!(h.script.borrow().opened, [(String::new(), BoxKind::Reply)]);
}

#[test]
fn should_start_the_box_from_the_saved_reply_when_the_file_has_one() {
    let source = format!("{QUESTIONS}\n<!-- limpid reply q2\nBecause.\n-->\n");
    let mut h = harness(&source, true);
    let src = h.field_src("q2");
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    assert_eq!(h.script.borrow().opened[0].0, "Because.");
}

#[test]
fn should_refuse_a_reply_box_when_the_stop_is_not_a_question() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    let result = h.fb.act(FeedbackAction::OpenReply, Some(&src));
    assert_eq!(result, ActionResult::Nothing("no question here".into()));
    assert!(!h.fb.box_open());
    assert_eq!(h.script.borrow().spawns, 0, "no editor for nothing");
}

#[test]
fn should_flash_editor_open_when_a_second_box_is_requested() {
    let mut h = harness(QUESTIONS, false);
    let q1 = h.field_src("q1");
    h.fb.act(FeedbackAction::OpenReply, Some(&q1));
    let again =
        h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert_eq!(
        again,
        ActionResult::Nothing("editor open: :wq or :q!".into())
    );
    let comment =
        h.fb.act(FeedbackAction::OpenComment, Some(&h.paragraph_src()));
    assert_eq!(
        comment,
        ActionResult::Nothing("editor open: :wq or :q!".into())
    );
    assert_eq!(h.script.borrow().opened.len(), 1);
}

#[test]
fn should_fail_without_a_box_when_the_editor_cannot_start() {
    let mut h = harness(QUESTIONS, false);
    h.script.borrow_mut().refuse_spawn = true;
    h.script.borrow_mut().running = false;
    let result =
        h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q1")));
    let ActionResult::Failed(message) = result else {
        panic!("expected a failure");
    };
    assert!(message.contains("no nvim"), "{message}");
    assert!(message.contains("[editor] command"), "{message}");
    assert!(!h.fb.box_open());
}

#[test]
fn should_reuse_the_running_editor_when_a_second_box_opens_later() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q1")));
    h.say(EditorEvent::Quit { discard: false });
    assert!(!h.fb.box_open());
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert_eq!(h.script.borrow().spawns, 1);
    assert_eq!(h.script.borrow().opened.len(), 2);
}

#[test]
fn should_restart_the_editor_when_it_died_between_boxes() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q1")));
    h.say(EditorEvent::Quit { discard: false });
    h.script.borrow_mut().running = false;
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert_eq!(h.script.borrow().spawns, 2);
    assert!(h.fb.box_open());
}

#[test]
fn should_store_the_buffer_as_the_reply_when_the_editor_saves() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.say(EditorEvent::Save("Because we must.\n".into()));
    assert_eq!(h.fb.pending().reply("q2"), Some("Because we must."));
    assert!(h.fb.box_open(), ":w keeps the box open");
    h.say(EditorEvent::Quit { discard: false });
    assert!(!h.fb.box_open());
    assert_eq!(
        h.fb.take_focus(),
        Some(Focus::Real(h.field_src("q2"))),
        "focus returns to the question"
    );
}

#[test]
fn should_keep_the_old_reply_when_the_box_closes_without_saving() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q1");
    h.fb.act(FeedbackAction::PickOption(1), Some(&src));
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    h.say(EditorEvent::Quit { discard: true });
    assert_eq!(h.fb.pending().reply("q1"), Some("SQLite"));
}

#[test]
fn should_show_the_box_in_the_overlay_while_it_is_open() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert!(h.fb.take_stale());
    h.rebuild();
    let src = h.fb.box_src().expect("a placeholder block for the box");
    assert_eq!(h.fb.resolve_focus(&Focus::Box), Some(src.clone()));
    assert_eq!(h.fb.classify(&src), StopRef::Box);
    h.say(EditorEvent::Quit { discard: false });
    h.rebuild();
    assert!(h.fb.box_src().is_none());
}

#[test]
fn should_follow_the_content_when_the_editor_redraws() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.fb.take_stale();
    let mut grid = Grid::new(40, 10);
    for (row, ch) in [(0usize, 'a'), (1, 'b'), (2, 'c'), (3, 'd'), (4, 'e')] {
        grid.cells[row * 40] = Cell { ch, hl: 0 };
    }
    h.script.borrow_mut().grid = Some(grid);
    h.say(EditorEvent::Redraw);
    assert_eq!(h.fb.open_box().unwrap().plan.rows, 6);
    assert!(
        h.fb.take_stale(),
        "the page must re-lay out for the new height"
    );
    h.say(EditorEvent::Redraw);
    assert!(!h.fb.take_stale(), "no change, no re-layout");
}

#[test]
fn should_track_mode_command_line_and_messages_when_the_editor_reports_them() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.say(EditorEvent::Mode("insert".into()));
    h.say(EditorEvent::CmdlineShow(":wq".into()));
    h.say(EditorEvent::Message("written".into()));
    let open = h.fb.open_box().unwrap();
    assert_eq!(open.mode, "insert");
    assert_eq!(open.cmdline.as_deref(), Some(":wq"));
    assert_eq!(h.fb.take_flashes(), ["written"]);
    h.say(EditorEvent::CmdlineHide);
    assert_eq!(h.fb.open_box().unwrap().cmdline, None);
    h.say(EditorEvent::Error("boom".into()));
    assert_eq!(h.fb.take_flashes(), ["editor: boom"]);
}

#[test]
fn should_ignore_editor_events_when_no_box_is_open() {
    let mut h = harness(QUESTIONS, false);
    h.say(EditorEvent::Save("stray".into()));
    h.say(EditorEvent::Quit { discard: true });
    h.say(EditorEvent::Mode("insert".into()));
    assert!(!h.fb.has_unsaved());
    assert!(h.fb.pending().replies.is_empty());
}

#[test]
fn should_forward_keys_to_the_editor_when_a_box_is_open() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.fb.input("<Esc>");
    h.fb.input("");
    assert_eq!(
        h.script.borrow().inputs,
        ["<Esc>"],
        "empty keys are not sent"
    );
}

#[test]
fn should_drop_keys_when_there_is_no_editor() {
    let mut h = harness(QUESTIONS, false);
    h.fb.input("a");
    assert!(h.script.borrow().inputs.is_empty());
}

#[test]
fn should_discard_the_buffer_and_close_when_force_closing() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.fb.force_close();
    assert!(!h.fb.box_open());
    assert!(
        h.script
            .borrow()
            .inputs
            .iter()
            .any(|keys| keys.contains(":q!")),
        "{:?}",
        h.script.borrow().inputs
    );
    assert_eq!(h.fb.take_flashes(), ["editor closed, buffer discarded"]);
    h.fb.force_close();
    assert!(
        h.fb.take_flashes().is_empty(),
        "nothing open, nothing to say"
    );
}

#[test]
fn should_close_the_box_when_the_editor_process_dies() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.script.borrow_mut().running = false;
    assert!(h.fb.tick(Instant::now()));
    assert!(!h.fb.box_open());
    assert_eq!(h.fb.take_flashes(), ["the editor exited"]);
    assert!(!h.fb.wants_ticks());
}

#[test]
fn should_stop_the_editor_when_it_has_been_idle_past_the_limit() {
    let mut h = harness_with("idle", QUESTIONS, false, "[editor]\nidle_seconds = 10\n");
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.say(EditorEvent::Quit { discard: false });
    assert!(h.fb.wants_ticks());
    h.fb.tick(Instant::now() + Duration::from_secs(5));
    assert!(h.fb.wants_ticks(), "not idle long enough");
    h.fb.tick(Instant::now() + Duration::from_secs(11));
    assert!(!h.fb.wants_ticks(), "stopped");
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert_eq!(h.script.borrow().spawns, 2, "respawned on the next box");
}

#[test]
fn should_not_stop_the_editor_while_a_box_is_open() {
    let mut h = harness_with("busy", QUESTIONS, false, "[editor]\nidle_seconds = 1\n");
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.fb.tick(Instant::now() + Duration::from_secs(3600));
    assert!(h.fb.box_open());
    assert!(h.fb.wants_ticks());
}

#[test]
fn should_save_to_the_sidecar_when_the_page_is_not_a_file_source() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q1");
    h.fb.act(FeedbackAction::PickOption(2), Some(&src));
    assert_eq!(h.fb.write(), ActionResult::Done("Saved".into()));
    assert!(!h.fb.has_unsaved());
    let sidecar = limpid_core::replies::sidecar_path(&h.page);
    let saved = AgentReplies::load(&sidecar).unwrap();
    assert_eq!(saved.replies["q1"], "Postgres");
    assert_eq!(
        std::fs::read_to_string(&h.page).unwrap(),
        QUESTIONS,
        "the page is untouched"
    );
    assert_eq!(
        h.fb.write(),
        ActionResult::Nothing("nothing to save".into())
    );
}

#[test]
fn should_show_what_the_sidecar_already_holds_when_the_page_opens() {
    let dir = scratch("preload");
    let page = dir.join("page.md");
    std::fs::write(&page, QUESTIONS).unwrap();
    let mut saved = AgentReplies::default();
    saved.replies.insert("q2".into(), "earlier".into());
    saved
        .save(&limpid_core::replies::sidecar_path(&page))
        .unwrap();
    let handoff = Handoff {
        path: page,
        theme: Scheme::Dark,
        pane: None,
        source: None,
        source_hash: None,
        source_backed: false,
    };
    let fb = Feedback::new(handoff, "t".into(), &Settings::default());
    assert_eq!(fb.pending().reply("q2"), Some("earlier"));
    assert!(!fb.has_unsaved(), "saved feedback is not unsaved");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_write_into_the_markdown_and_ask_for_a_reload_when_the_page_is_a_file() {
    let mut h = harness(QUESTIONS, true);
    assert!(h.fb.is_file_backed());
    let src = h.field_src("q1");
    h.fb.act(FeedbackAction::PickOption(1), Some(&src));
    assert_eq!(h.fb.write(), ActionResult::Done("Saved".into()));
    let written = std::fs::read_to_string(&h.page).unwrap();
    assert!(written.starts_with("# Plan"), "{written}");
    assert!(written.contains("limpid reply q1"), "{written}");
    assert!(written.contains("SQLite"), "{written}");
    assert!(h.fb.take_reload());
    assert!(!h.fb.take_reload(), "the request is consumed");
    assert!(
        h.fb.pending().replies.is_empty(),
        "the file is now the record"
    );
}

#[test]
fn should_refuse_to_save_when_the_file_changed_on_disk() {
    let mut h = harness(QUESTIONS, true);
    let src = h.field_src("q1");
    h.fb.act(FeedbackAction::PickOption(1), Some(&src));
    std::fs::write(&h.page, format!("{QUESTIONS}\nExternal edit.\n")).unwrap();
    let result = h.fb.write();
    assert!(matches!(result, ActionResult::Failed(_)), "{result:?}");
    assert!(h.fb.has_unsaved(), "nothing is lost when a save fails");
    assert!(
        !std::fs::read_to_string(&h.page)
            .unwrap()
            .contains("limpid reply"),
        "the external edit is left alone"
    );
}

#[test]
fn should_save_again_after_reloading_when_the_file_was_changed_outside() {
    let mut h = harness(QUESTIONS, true);
    let src = h.field_src("q1");
    h.fb.act(FeedbackAction::PickOption(1), Some(&src));
    std::fs::write(&h.page, format!("{QUESTIONS}\nExternal edit.\n")).unwrap();
    assert!(matches!(h.fb.write(), ActionResult::Failed(_)));
    h.reload();
    assert_eq!(h.fb.write(), ActionResult::Done("Saved".into()));
}

#[test]
fn should_refuse_to_quit_while_anything_is_unsaved() {
    let mut h = harness(QUESTIONS, false);
    assert!(!h.fb.has_unsaved());
    h.fb.act(FeedbackAction::PickOption(1), Some(&h.field_src("q1")));
    assert!(h.fb.has_unsaved());
    h.fb.write();
    assert!(!h.fb.has_unsaved());
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert!(h.fb.has_unsaved(), "an open box counts");
}

#[test]
fn should_save_and_deliver_when_writing_and_sending() {
    let dir = scratch("send-target");
    let delivered = dir.join("delivered.txt");
    let settings = format!("[reply]\ncommand = \"cat > '{}'\"\n", delivered.display());
    let mut h = harness_with("send", QUESTIONS, false, &settings);
    h.fb.act(FeedbackAction::PickOption(2), Some(&h.field_src("q1")));
    let result = h.fb.write_and_send();
    assert_eq!(result, ActionResult::Done("Sent".into()));
    let message = std::fs::read_to_string(&delivered).unwrap();
    assert!(message.starts_with("re: Plan"), "{message}");
    assert!(message.contains("q1: Postgres"), "{message}");
    assert!(
        limpid_core::replies::sidecar_path(&h.page).exists(),
        "saved before delivery"
    );
    assert!(!h.fb.has_unsaved());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_keep_the_feedback_saved_when_delivery_fails() {
    let mut h = harness_with("fail", QUESTIONS, false, "[reply]\ncommand = \"exit 3\"\n");
    h.fb.act(FeedbackAction::PickOption(1), Some(&h.field_src("q1")));
    let result = h.fb.write_and_send();
    let ActionResult::Failed(message) = result else {
        panic!("expected failure, got {result:?}");
    };
    assert!(message.contains("saved"), "{message}");
    assert!(limpid_core::replies::sidecar_path(&h.page).exists());
    assert!(!h.fb.has_unsaved(), "the save went through");
}

#[test]
fn should_send_nothing_when_there_is_no_feedback() {
    let mut h = harness(QUESTIONS, false);
    assert_eq!(
        h.fb.write_and_send(),
        ActionResult::Nothing("no feedback to send".into())
    );
}

#[test]
fn should_send_nothing_when_every_reply_is_blank() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.say(EditorEvent::Save("   \n".into()));
    assert_eq!(
        h.fb.write_and_send(),
        ActionResult::Nothing("no feedback to send".into())
    );
}

#[test]
fn should_comment_on_a_block_with_a_quote_and_lines_when_the_editor_saves() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    assert_eq!(
        h.script.borrow().opened,
        [(String::new(), BoxKind::Comment)]
    );
    h.say(EditorEvent::Save("This is vague.\n".into()));
    let key = Feedback::comment_key(&src);
    let entry = &h.fb.pending().comments[&key];
    assert_eq!(entry.text, "This is vague.");
    assert_eq!(entry.quote, "Intro paragraph.");
    assert_eq!((entry.start_line, entry.end_line), (3, 3));
    assert_eq!(entry.target, src);
    h.say(EditorEvent::Quit { discard: false });
    assert_eq!(h.fb.take_focus(), Some(Focus::Comment(key.clone())));
    h.rebuild();
    let shown = h.fb.resolve_focus(&Focus::Comment(key.clone())).unwrap();
    assert_eq!(h.fb.classify(&shown), StopRef::Comment(key));
}

#[test]
fn should_save_a_comment_in_the_sidecar_under_the_block_range() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    h.say(EditorEvent::Save("Check this.".into()));
    h.say(EditorEvent::Quit { discard: false });
    h.fb.write();
    let saved = AgentReplies::load(&limpid_core::replies::sidecar_path(&h.page)).unwrap();
    let comment = &saved.comments[&Feedback::comment_key(&src)];
    assert_eq!(comment.text, "Check this.");
    assert_eq!(comment.quote, "Intro paragraph.");
}

#[test]
fn should_edit_the_existing_comment_when_commenting_on_a_commented_block() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    h.say(EditorEvent::Save("first".into()));
    h.say(EditorEvent::Quit { discard: false });
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    assert_eq!(h.script.borrow().opened[1].0, "first");
}

#[test]
fn should_remove_a_comment_that_is_saved_empty() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    h.say(EditorEvent::Save("temporary".into()));
    h.say(EditorEvent::Save("".into()));
    assert!(h.fb.pending().comments.is_empty());
}

#[test]
fn should_comment_on_a_question_as_a_block_when_c_is_pressed_on_a_field() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q1");
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    h.say(EditorEvent::Save("Neither, use files.".into()));
    assert!(
        h.fb.pending()
            .comments
            .contains_key(&Feedback::comment_key(&src))
    );
}

#[test]
fn should_delete_a_saved_comment_from_the_file_when_x_is_pressed_and_saved() {
    let source = "para one\n\n<!-- limpid comment\nleaks\n-->\n\npara two\n";
    let mut h = harness(source, true);
    let note = h.fb.index.notes[0].src.clone();
    let result = h.fb.act(FeedbackAction::DeleteComment, Some(&note));
    assert!(matches!(result, ActionResult::Done(_)), "{result:?}");
    assert!(h.fb.has_unsaved());
    assert_eq!(h.fb.write(), ActionResult::Done("Saved".into()));
    let written = std::fs::read_to_string(&h.page).unwrap();
    assert!(!written.contains("limpid comment"), "{written}");
    assert!(written.contains("para one") && written.contains("para two"));
}

#[test]
fn should_delete_a_sidecar_comment_when_x_is_pressed_and_saved() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    h.say(EditorEvent::Save("bad".into()));
    h.say(EditorEvent::Quit { discard: false });
    h.fb.write();
    h.rebuild();
    let shown =
        h.fb.resolve_focus(&Focus::Comment(Feedback::comment_key(&src)))
            .unwrap();
    let result = h.fb.act(FeedbackAction::DeleteComment, Some(&shown));
    assert!(matches!(result, ActionResult::Done(_)), "{result:?}");
    h.fb.write();
    let saved = AgentReplies::load(&limpid_core::replies::sidecar_path(&h.page)).unwrap();
    assert!(saved.comments.is_empty());
    assert_eq!(h.fb.take_focus(), Some(Focus::Real(src)));
}

#[test]
fn should_refuse_to_delete_when_the_stop_is_not_a_comment() {
    let mut h = harness(QUESTIONS, false);
    let result =
        h.fb.act(FeedbackAction::DeleteComment, Some(&h.paragraph_src()));
    assert_eq!(result, ActionResult::Nothing("no comment here".into()));
    assert!(!h.fb.has_unsaved());
}

#[test]
fn should_edit_a_saved_file_comment_in_place_and_replace_it_on_save() {
    let source = "para one\n\n<!-- limpid comment\nleaks\n-->\n\npara two\n";
    let mut h = harness(source, true);
    let note = h.fb.index.notes[0].src.clone();
    h.fb.act(FeedbackAction::OpenComment, Some(&note));
    assert_eq!(h.script.borrow().opened[0].0, "leaks");
    h.say(EditorEvent::Save("leaks on error".into()));
    h.say(EditorEvent::Quit { discard: false });
    h.fb.write();
    let written = std::fs::read_to_string(&h.page).unwrap();
    assert!(written.contains("leaks on error"), "{written}");
    assert_eq!(written.matches("limpid comment").count(), 1, "{written}");
}

#[test]
fn should_write_a_comment_into_the_file_and_reload_it_as_an_annotation() {
    let mut h = harness(QUESTIONS, true);
    let src = h.paragraph_src();
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    h.say(EditorEvent::Save("Say more.".into()));
    h.say(EditorEvent::Quit { discard: false });
    h.fb.write();
    h.reload();
    let notes = &h.fb.index.notes;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text.trim(), "Say more.");
    assert_eq!(notes[0].kind, limpid_core::doc::AnnotationKind::Comment);
}

#[test]
fn should_find_the_first_unanswered_question_when_t_is_pressed() {
    let mut h = harness(QUESTIONS, false);
    let result = h.fb.act(FeedbackAction::FirstUnanswered, None);
    assert_eq!(result, ActionResult::Done("2 unanswered, first q1".into()));
    assert_eq!(h.fb.take_focus(), Some(Focus::Real(h.field_src("q1"))));
    h.fb.act(FeedbackAction::PickOption(1), Some(&h.field_src("q1")));
    h.fb.take_focus();
    let result = h.fb.act(FeedbackAction::FirstUnanswered, None);
    assert_eq!(result, ActionResult::Done("1 unanswered, first q2".into()));
    assert_eq!(h.fb.take_focus(), Some(Focus::Real(h.field_src("q2"))));
}

#[test]
fn should_count_a_saved_file_reply_as_an_answer_when_looking_for_unanswered() {
    let mut h = harness(QUESTIONS, true);
    h.fb.act(FeedbackAction::PickOption(1), Some(&h.field_src("q1")));
    assert_eq!(h.fb.write(), ActionResult::Done("Saved".into()));
    h.reload();
    assert!(
        h.fb.pending().replies.is_empty(),
        "only the file holds it now"
    );
    let result = h.fb.act(FeedbackAction::FirstUnanswered, None);
    assert_eq!(result, ActionResult::Done("1 unanswered, first q2".into()));
}

#[test]
fn should_report_all_answered_when_nothing_is_left() {
    let mut h = harness("just prose\n", false);
    assert_eq!(
        h.fb.act(FeedbackAction::FirstUnanswered, None),
        ActionResult::Nothing("all questions answered".into())
    );
}

#[test]
fn should_open_a_review_of_the_pending_message_when_s_is_pressed() {
    let mut h = harness(QUESTIONS, false);
    assert_eq!(
        h.fb.act(FeedbackAction::Review, None),
        ActionResult::Nothing("no feedback yet".into())
    );
    h.fb.act(FeedbackAction::PickOption(1), Some(&h.field_src("q1")));
    let result = h.fb.act(FeedbackAction::Review, None);
    assert!(matches!(result, ActionResult::Done(_)), "{result:?}");
    let review = h.fb.review().expect("the overlay is open");
    assert!(review.message.starts_with("re: Plan"), "{}", review.message);
    assert!(review.message.contains("q1: SQLite"), "{}", review.message);
    h.fb.close_review();
    assert!(h.fb.review().is_none());
}

#[test]
fn should_include_saved_file_feedback_in_the_review_when_more_is_pending() {
    let source = format!("{QUESTIONS}\n<!-- limpid reply q2\nBecause.\n-->\n");
    let mut h = harness(&source, true);
    h.fb.act(FeedbackAction::PickOption(2), Some(&h.field_src("q1")));
    h.fb.act(FeedbackAction::Review, None);
    let message = &h.fb.review().unwrap().message;
    assert!(message.contains("q1: Postgres"), "{message}");
    assert!(message.contains("q2: Because."), "{message}");
}

#[test]
fn should_hold_the_stop_identity_across_a_rebuild_when_a_comment_appears_above() {
    let mut h = harness(QUESTIONS, false);
    let field = h.field_src("q1");
    let before = h.fb.identify(&field);
    h.fb.act(FeedbackAction::OpenComment, Some(&h.paragraph_src()));
    h.say(EditorEvent::Save("above".into()));
    h.say(EditorEvent::Quit { discard: false });
    h.rebuild();
    assert_eq!(before, Focus::Real(field.clone()));
    assert_eq!(h.fb.resolve_focus(&before), Some(field));
}

#[test]
fn should_pass_the_configured_command_and_size_to_the_spawner() {
    let dir = scratch("config");
    let page = dir.join("p.md");
    std::fs::write(&page, QUESTIONS).unwrap();
    let settings = Settings::parse("[editor]\ncommand = '/opt/nvim'\ninit = 'clean'\n").unwrap();
    let seen: Rc<RefCell<Vec<EditorConfig>>> = Rc::default();
    let seen_in = seen.clone();
    let spawner: EditorSpawner = Box::new(move |config| {
        seen_in.borrow_mut().push(config.clone());
        Err(EditorError::Spawn(std::io::Error::other("stop here")))
    });
    let handoff = Handoff {
        path: page,
        theme: Scheme::Dark,
        pane: None,
        source: None,
        source_hash: None,
        source_backed: false,
    };
    let mut fb = Feedback::new(handoff, "t".into(), &settings).with_spawner(spawner);
    let doc = parse(QUESTIONS, Mode::File, &settings);
    fb.index_document(&doc.blocks, &doc.source);
    fb.overlay(&doc.blocks);
    let field = fb.index.fields[0].src.clone();
    fb.act(FeedbackAction::OpenReply, Some(&field));
    let configs = seen.borrow();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].command, "/opt/nvim");
    assert_eq!(configs[0].init, crate::editor::InitMode::Clean);
    assert!(configs[0].cols > 0 && configs[0].rows > 0);
    drop(configs);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn should_start_from_sane_options_when_built_from_settings() {
    let options = FeedbackOptions::from_settings(&Settings::default());
    assert_eq!(options.idle, Duration::from_secs(300));
    assert_eq!(options.quote_chars, 100);
    assert!(options.limits.min_rows <= options.limits.max_rows);
}

#[test]
fn should_flash_a_problem_when_the_sidecar_cannot_be_read() {
    let dir = scratch("badsidecar");
    let page = dir.join("p.md");
    std::fs::write(&page, QUESTIONS).unwrap();
    std::fs::write(limpid_core::replies::sidecar_path(&page), "{ not json").unwrap();
    let handoff = Handoff {
        path: page,
        theme: Scheme::Dark,
        pane: None,
        source: None,
        source_hash: None,
        source_backed: false,
    };
    let mut fb = Feedback::new(handoff, "t".into(), &Settings::default());
    let flashes = fb.take_flashes();
    assert_eq!(flashes.len(), 1);
    assert!(
        flashes[0].contains("could not read saved feedback"),
        "{flashes:?}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- one editor per process, idle ticks, key path, IME ----

/// A second window's feedback sharing `h`'s editor host.
fn second_window(h: &Harness) -> Feedback {
    let settings = Settings::default();
    let handoff = Handoff {
        path: h.page.clone(),
        theme: Scheme::Dark,
        pane: None,
        source: None,
        source_hash: None,
        source_backed: false,
    };
    let mut other = Feedback::new(handoff, "Other".into(), &settings).with_host(h.fb.host.clone());
    let doc = parse(QUESTIONS, Mode::File, &settings);
    other.index_document(&doc.blocks, &doc.source);
    other.overlay(&doc.blocks);
    other
}

#[test]
fn should_start_one_editor_when_two_windows_open_boxes_in_turn() {
    let mut h = harness(QUESTIONS, false);
    let mut other = second_window(&h);
    let src = h.field_src("q2");
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    h.say(EditorEvent::Quit { discard: false });
    let result = other.act(FeedbackAction::OpenReply, Some(&src));
    assert_eq!(result, ActionResult::Done(String::new()));
    assert_eq!(h.script.borrow().spawns, 1, "the second window reuses nvim");
    assert_eq!(h.script.borrow().opened.len(), 2);
}

#[test]
fn should_refuse_a_second_box_when_another_window_has_the_editor() {
    let mut h = harness(QUESTIONS, false);
    let mut other = second_window(&h);
    let src = h.field_src("q2");
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    let result = other.act(FeedbackAction::OpenReply, Some(&src));
    assert_eq!(
        result,
        ActionResult::Nothing("editor in use in another window".into())
    );
    assert!(!other.box_open());
    assert!(h.fb.box_open(), "the first box is undisturbed");
    assert_eq!(h.script.borrow().opened.len(), 1);
}

#[test]
fn should_not_hand_one_windows_events_to_another_when_both_tick() {
    let mut h = harness(QUESTIONS, false);
    let mut other = second_window(&h);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.script
        .borrow_mut()
        .events
        .push_back(EditorEvent::Save("mine".into()));
    other.tick(Instant::now());
    assert_eq!(
        h.script.borrow().events.len(),
        1,
        "the bystander must not consume the owner's events"
    );
    h.fb.tick(Instant::now());
    assert_eq!(h.fb.pending().reply("q2"), Some("mine"));
}

#[test]
fn should_let_the_other_window_open_when_the_first_box_closes() {
    let mut h = harness(QUESTIONS, false);
    let mut other = second_window(&h);
    let src = h.field_src("q2");
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    h.fb.force_close();
    let result = other.act(FeedbackAction::OpenReply, Some(&src));
    assert_eq!(result, ActionResult::Done(String::new()));
}

#[test]
fn should_stop_the_shared_editor_for_every_window_when_idle() {
    let mut h = harness_with("idle2", QUESTIONS, false, "[editor]\nidle_seconds = 10\n");
    let other = second_window(&h);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.say(EditorEvent::Quit { discard: false });
    assert!(other.wants_ticks(), "the idle timer is process-wide");
    h.fb.tick(Instant::now() + Duration::from_secs(11));
    assert!(!other.wants_ticks());
    assert!(!h.fb.wants_ticks());
}

/// A port whose answers come late: any call to `poll` costs `lag`.
struct Late {
    lag: Duration,
    polls: Rc<std::cell::Cell<usize>>,
    sent: Rc<RefCell<Vec<String>>>,
    grid: Grid,
}

impl EditorPort for Late {
    fn open(&mut self, _text: &str, _kind: BoxKind) -> Result<(), EditorError> {
        Ok(())
    }
    fn input(&mut self, keys: &str) {
        self.sent.borrow_mut().push(keys.to_owned());
    }
    fn resize(&mut self, _cols: u16, _rows: u16) {}
    fn poll(&mut self) -> Vec<EditorEvent> {
        self.polls.set(self.polls.get() + 1);
        std::thread::sleep(self.lag);
        Vec::new()
    }
    fn grid(&self) -> &Grid {
        &self.grid
    }
    fn is_running(&mut self) -> bool {
        true
    }
}

#[test]
fn should_return_from_input_at_once_when_the_editor_answers_late() {
    let polls = Rc::new(std::cell::Cell::new(0));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let (late_polls, late_sent) = (polls.clone(), sent.clone());
    let spawner: EditorSpawner = Box::new(move |_| {
        Ok(Box::new(Late {
            lag: Duration::from_millis(400),
            polls: late_polls.clone(),
            sent: late_sent.clone(),
            grid: Grid::new(40, 6),
        }) as Box<dyn EditorPort>)
    });
    let mut h = harness(QUESTIONS, false);
    h.fb = Feedback::new(h.fb.handoff().clone(), "x".into(), &Settings::default())
        .with_spawner(spawner);
    let doc = parse(QUESTIONS, Mode::File, &Settings::default());
    h.fb.index_document(&doc.blocks, &doc.source);
    h.fb.overlay(&doc.blocks);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    let started = Instant::now();
    for _ in 0..5 {
        h.fb.input("a");
    }
    let spent = started.elapsed();
    assert!(
        spent < Duration::from_millis(100),
        "five keys took {spent:?}"
    );
    assert_eq!(polls.get(), 0, "the key path never waits on the editor");
    assert_eq!(sent.borrow().len(), 5);
}

#[test]
fn should_tick_every_frame_only_while_a_box_is_open() {
    let mut h = harness_with("period", QUESTIONS, false, "[editor]\nidle_seconds = 300\n");
    let now = Instant::now();
    assert!(
        h.fb.tick_period(now) >= Duration::from_secs(1),
        "no editor, no fast ticks"
    );
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert_eq!(h.fb.tick_period(now), TICK_INTERVAL);
    h.say(EditorEvent::Quit { discard: false });
    let period = h.fb.tick_period(Instant::now());
    assert!(
        period > Duration::from_secs(250) && period <= Duration::from_secs(300),
        "idle period should run to the deadline: {period:?}"
    );
}

#[test]
fn should_wake_in_a_millisecond_not_spin_when_the_deadline_has_passed() {
    let mut h = harness_with("past", QUESTIONS, false, "[editor]\nidle_seconds = 1\n");
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.say(EditorEvent::Quit { discard: false });
    let period = h.fb.tick_period(Instant::now() + Duration::from_secs(60));
    assert_eq!(period, Duration::from_millis(1));
}

#[test]
fn should_sleep_long_when_another_windows_box_has_the_editor() {
    let mut h = harness(QUESTIONS, false);
    let other = second_window(&h);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert!(other.tick_period(Instant::now()) >= Duration::from_secs(1));
}

#[test]
fn should_type_committed_text_into_the_box_when_an_ime_commits() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.fb.ime_commit("a<b>\u{3042}");
    assert_eq!(h.script.borrow().inputs, ["a<lt>b>\u{3042}"]);
}

#[test]
fn should_ignore_an_ime_commit_when_no_box_is_open_or_the_text_is_empty() {
    let mut h = harness(QUESTIONS, false);
    h.fb.ime_commit("x");
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    h.fb.ime_commit("");
    assert!(h.script.borrow().inputs.is_empty());
}

#[test]
fn should_show_the_composition_in_the_status_line_while_preediting() {
    let mut h = harness(QUESTIONS, false);
    assert!(!h.fb.ime_preedit(Some("ka")), "no box, nothing to show");
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    assert!(h.fb.ime_preedit(Some("ka")));
    assert!(!h.fb.ime_preedit(Some("ka")), "unchanged is not a repaint");
    assert_eq!(h.fb.open_box().unwrap().status_label(), "ka | NORMAL");
    assert!(h.fb.ime_preedit(Some("")));
    assert_eq!(h.fb.open_box().unwrap().status_label(), "NORMAL");
}

#[test]
fn should_escape_only_less_than_when_converting_ime_text() {
    assert_eq!(ime_keys("<Esc> & <lt>"), "<lt>Esc> & <lt>lt>");
    assert_eq!(ime_keys("plain"), "plain");
    assert_eq!(ime_keys(""), "");
}

#[test]
fn should_let_another_window_open_when_the_owner_is_dropped_with_its_box_open() {
    let mut h = harness(QUESTIONS, false);
    let mut other = second_window(&h);
    let src = h.field_src("q2");
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    assert_eq!(
        other.act(FeedbackAction::OpenReply, Some(&src)),
        ActionResult::Nothing("editor in use in another window".into())
    );
    let host = h.fb.host.clone();
    drop(h);
    assert!(!host.borrow().in_use_by_other(other.owner));
    let result = other.act(FeedbackAction::OpenReply, Some(&src));
    assert_eq!(result, ActionResult::Done(String::new()));
}

#[test]
fn should_resize_the_editor_when_ownership_passes_to_a_window_with_its_old_size() {
    let mut h = harness(QUESTIONS, false);
    let other = second_window(&h);
    let src = h.field_src("q2");
    let host = h.fb.host.clone();
    let size_for = |fb: &Feedback, cols: u16| {
        fb.host.borrow_mut().resize(fb.owner, cols, 6);
    };
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    size_for(&h.fb, 100);
    h.fb.force_close();
    let mut other = other;
    other.act(FeedbackAction::OpenReply, Some(&src));
    size_for(&other, 60);
    other.force_close();
    h.fb.act(FeedbackAction::OpenReply, Some(&src));
    size_for(&h.fb, 100);
    drop(host);
    assert_eq!(h.script.borrow().resizes, vec![(100, 6), (60, 6), (100, 6)]);
}

#[test]
fn should_not_resend_the_size_when_the_same_window_paints_again() {
    let mut h = harness(QUESTIONS, false);
    h.fb.act(FeedbackAction::OpenReply, Some(&h.field_src("q2")));
    for _ in 0..3 {
        h.fb.host.borrow_mut().resize(h.fb.owner, 80, 6);
    }
    assert_eq!(h.script.borrow().resizes, vec![(80, 6)]);
}

#[test]
fn should_send_insert_at_start_keys_when_capital_i_opens_a_comment() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    let result = h.fb.act(FeedbackAction::Edit(EditAt::Start), Some(&src));
    assert!(matches!(result, ActionResult::Done(_)), "{result:?}");
    assert_eq!(h.script.borrow().opened[0].1, BoxKind::Comment);
    assert_eq!(h.script.borrow().inputs, ["<C-\\><C-n>ggI"]);
}

#[test]
fn should_send_append_at_end_keys_when_capital_a_opens_a_reply() {
    let mut h = harness(QUESTIONS, false);
    let src = h.field_src("q2");
    h.fb.act(FeedbackAction::Edit(EditAt::End), Some(&src));
    assert_eq!(h.script.borrow().opened[0].1, BoxKind::Reply);
    assert_eq!(h.script.borrow().inputs, ["<C-\\><C-n>GA"]);
}

#[test]
fn should_send_no_keys_when_plain_i_opens_a_box() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    h.fb.act(FeedbackAction::Edit(EditAt::Keep), Some(&src));
    assert!(h.fb.box_open());
    assert!(h.script.borrow().inputs.is_empty());
}

#[test]
fn should_edit_the_comment_when_enter_is_pressed_on_a_comment() {
    let mut h = harness(QUESTIONS, false);
    let src = h.paragraph_src();
    h.fb.act(FeedbackAction::OpenComment, Some(&src));
    h.say(EditorEvent::Save("Vague.".into()));
    h.say(EditorEvent::Quit { discard: false });
    h.rebuild();
    let key = Feedback::comment_key(&src);
    let shown = h.fb.resolve_focus(&Focus::Comment(key)).unwrap();
    let result = h.fb.act(FeedbackAction::OpenReply, Some(&shown));
    assert!(matches!(result, ActionResult::Done(_)), "{result:?}");
    assert_eq!(
        h.script.borrow().opened.last().unwrap(),
        &("Vague.".to_owned(), BoxKind::Comment)
    );
}
