//! Running a command: the request that goes out, and the edit that comes back.
//!
//! The process itself belongs to the session — `crates/inkstone-server` is where
//! one is really spawned, and `docs/jobs.md` says why — so what is tested here
//! is the kernel's half of the bargain: what is asked for, what is done with the
//! answer, and what happens when the answer arrives too late.

use super::*;
use crate::buffer::Buffer;

#[test]
fn a_shell_command_is_asked_for_with_no_input() {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("!echo hi");

    assert_eq!(ed.message, "!echo hi");
    let request = ed.job_request().expect("the session is asked to run it");
    assert_eq!(request.command, "echo hi");
    assert_eq!(request.input, None, "nothing to write to its input");
    // Asked for once: the session takes the request, and the kernel keeps the
    // half that says what to do with the answer.
    assert!(ed.job_request().is_none(), "and asked for once");
}

#[test]
fn the_output_of_a_command_becomes_a_document_of_its_own() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("the file\n");
    ed.execute_cmdline("!echo hi");
    ed.job_request();

    let windows = ed.windows.len();
    ed.job_finished("hi\n".into(), String::new(), Some(0));

    assert_eq!(ed.windows.len(), windows + 1, "a window to read it in");
    assert_eq!(
        ed.buffer.text(),
        "the file\n",
        "the file is where the cursor was, and stays"
    );
    let shown = ed.documents.last().expect("a document arrived");
    assert_eq!(shown.buffer.text(), "hi\n");
    assert_eq!(shown.buffer.display_name(), "!echo hi");
    assert_eq!(ed.message, "!echo hi");
}

#[test]
fn both_streams_go_into_the_document_because_a_builds_errors_are_on_the_second() {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("!cargo build");
    ed.job_request();
    ed.job_finished("Compiling\n".into(), "error: nope\n".into(), Some(101));

    let shown = ed.documents.last().expect("a document arrived");
    assert_eq!(shown.buffer.text(), "Compiling\nerror: nope\n");
    // And the message says what happened, with the reason in the first line of
    // what the command had to complain about.
    assert_eq!(ed.message, "shell returned 101: error: nope");
}

#[test]
fn a_command_that_says_nothing_opens_no_window() {
    let mut ed = Editor::for_tests();
    let windows = ed.windows.len();
    ed.execute_cmdline("!true");
    ed.job_request();
    ed.job_finished(String::new(), String::new(), Some(0));

    assert_eq!(ed.windows.len(), windows, "nothing to show");
    assert_eq!(ed.message, "!true");
}

#[test]
fn a_command_that_failed_and_says_nothing_still_opens_no_window() {
    // What happened is the message row's business; an empty window is a window
    // nobody asked for, however the command ended.
    let mut ed = Editor::for_tests();
    let windows = ed.windows.len();
    ed.execute_cmdline("!false");
    ed.job_request();
    ed.job_finished(String::new(), String::new(), Some(1));

    assert_eq!(ed.windows.len(), windows);
    assert_eq!(ed.message, "shell returned 1");
}

#[test]
fn read_command_puts_the_output_below_the_line_it_was_asked_from() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.cursor.row = 0;
    ed.execute_cmdline("r !echo hi");

    let request = ed.job_request().unwrap();
    assert_eq!(request.command, "echo hi");
    ed.job_finished("hi\n".into(), String::new(), Some(0));

    assert_eq!(ed.buffer.text(), "one\nhi\ntwo\n");
    assert_eq!(ed.message, "1 line read from \"!echo hi\"");
}

#[test]
fn an_output_that_does_not_end_in_a_newline_is_still_whole_lines() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.execute_cmdline("r !printf hi");
    ed.job_request();
    ed.job_finished("hi".into(), String::new(), Some(0));

    assert_eq!(ed.buffer.text(), "one\nhi\n");
}

#[test]
fn a_read_of_nothing_says_nothing_came() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\n");
    ed.execute_cmdline("r !true");
    ed.job_request();
    ed.job_finished(String::new(), String::new(), Some(0));

    assert_eq!(ed.buffer.text(), "one\n");
    assert_eq!(ed.message, "0 lines read from \"!true\"");
}

#[test]
fn a_filter_takes_the_range_in_and_puts_the_answer_back() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("pear\napple\n");
    ed.execute_cmdline("%!sort");

    let request = ed.job_request().unwrap();
    assert_eq!(request.command, "sort");
    assert_eq!(request.input.as_deref(), Some("pear\napple\n"));
    ed.job_finished("apple\npear\n".into(), String::new(), Some(0));

    assert_eq!(ed.buffer.text(), "apple\npear\n");
    assert_eq!(ed.message, "2 lines filtered");
    // A filter is one change, so one `u` puts the text back: it goes through the
    // same door every other edit does.
    feed(&mut ed, "u");
    assert_eq!(ed.buffer.text(), "pear\napple\n");
}

#[test]
fn a_range_narrows_what_a_filter_is_given() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("keep\npear\napple\nkeep too\n");
    ed.execute_cmdline("2,3!sort");

    let request = ed.job_request().unwrap();
    assert_eq!(request.input.as_deref(), Some("pear\napple\n"));
    ed.job_finished("apple\npear\n".into(), String::new(), Some(0));

    assert_eq!(ed.buffer.text(), "keep\napple\npear\nkeep too\n");
}

#[test]
fn an_answer_that_arrived_after_the_text_moved_on_is_refused() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("pear\napple\n");
    ed.execute_cmdline("%!sort");
    ed.job_request();
    // Somebody goes on editing the range while the command runs — which is what
    // running it off the editor's thread buys, and what this has to pay for.
    let moved = ed.buffer.line_col_to_char(0, 0);
    ed.buffer = Buffer::from_text("plum\napple\n");
    let _ = moved;

    ed.job_finished("apple\npear\n".into(), String::new(), Some(0));

    assert_eq!(ed.buffer.text(), "plum\napple\n", "nothing was replaced");
    assert_eq!(ed.message, "E: the text changed while \"sort\" ran");
}

#[test]
fn a_read_whose_line_moved_on_is_refused_the_same_way() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\ntwo\n");
    ed.execute_cmdline("r !echo hi");
    ed.job_request();
    ed.buffer = Buffer::from_text("different\n");
    ed.job_finished("hi\n".into(), String::new(), Some(0));

    assert_eq!(ed.buffer.text(), "different\n");
    assert_eq!(ed.message, "E: the text changed while \"echo hi\" ran");
}

#[test]
fn one_command_at_a_time() {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("!sleep 1");
    assert_eq!(ed.message, "!sleep 1");
    let first = ed.job_request().expect("the session took the first one");
    assert_eq!(first.command, "sleep 1");

    ed.execute_cmdline("!echo hi");
    assert_eq!(ed.message, "E: sleep 1 is still running (:kill stops it)");
    assert!(
        ed.job_request().is_none(),
        "and the second one was never asked for"
    );
}

#[test]
fn kill_asks_for_the_end_of_the_running_one_and_is_taken_once() {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("kill");
    assert_eq!(ed.message, "E: no command is running");

    ed.execute_cmdline("!sleep 1");
    ed.execute_cmdline("kill");
    assert!(ed.take_job_kill(), "the session is told");
    assert!(!ed.take_job_kill(), "and told once");
}

#[test]
fn a_command_line_with_no_command_is_an_error() {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("!");
    assert_eq!(ed.message, "E471: Argument required");
    assert!(ed.job_request().is_none());

    ed.execute_cmdline("r !");
    assert_eq!(ed.message, "E471: Argument required");
    assert!(ed.job_request().is_none());
}

#[test]
fn a_command_that_was_never_started_is_told_so() {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("!nosuchthing");
    ed.job_request();
    ed.job_failed("program not found".to_string());

    assert_eq!(
        ed.message,
        "E: cannot run \"nosuchthing\": program not found"
    );
    // The editor is free again: a job that never started is not a job.
    assert!(ed.job_request().is_none());
    ed.execute_cmdline("!echo hi");
    assert!(ed.job_request().is_some());
}

#[test]
fn a_program_does_not_get_to_change_the_files_line_endings() {
    // `sort` on Windows ends every line with CRLF, and a file that was LF must
    // not come back from `:%!sort` as one that was not: what a filter replaces
    // is the text, and the terminator belongs to the line model.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("pear\napple\n");
    ed.execute_cmdline("%!sort");
    ed.job_request();
    ed.job_finished("apple\r\npear\r\n".into(), String::new(), Some(0));
    assert_eq!(ed.buffer.text(), "apple\npear\n");

    // And the other way round: a file written with CRLF keeps it.
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("pear\r\napple\r\n");
    ed.execute_cmdline("%!sort");
    ed.job_request();
    ed.job_finished("apple\npear\n".into(), String::new(), Some(0));
    assert_eq!(ed.buffer.text(), "apple\r\npear\r\n");
}

#[test]
fn a_read_inserts_lines_that_end_the_way_the_line_above_them_does() {
    let mut ed = Editor::for_tests();
    ed.buffer = Buffer::from_text("one\r\ntwo\r\n");
    ed.cursor.row = 0;
    ed.execute_cmdline("r !echo hi");
    ed.job_request();
    ed.job_finished("hi\n".into(), String::new(), Some(0));

    assert_eq!(ed.buffer.text(), "one\r\nhi\r\ntwo\r\n");
}

#[test]
fn a_killed_command_says_so_rather_than_naming_a_code() {
    let mut ed = Editor::for_tests();
    ed.execute_cmdline("!sleep 100");
    ed.job_request();
    ed.job_finished(String::new(), String::new(), None);

    assert_eq!(ed.message, "shell killed");
}
