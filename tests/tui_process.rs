//! Exercise the real terminal/application stack, not just the input reducer.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

struct TuiProcess {
    child: Box<dyn Child + Send + Sync>,
    writer: Option<Box<dyn Write + Send>>,
    master: Option<Box<dyn MasterPty + Send>>,
    output: Arc<Mutex<Vec<u8>>>,
    reader: Option<thread::JoinHandle<()>>,
    terminal_reply_offset: usize,
    parsed_output_offset: usize,
    screen: vt100::Parser,
}

impl TuiProcess {
    fn start(home: &Path, workspace: &Path, args: &[&str]) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 35,
                cols: 120,
                ..PtySize::default()
            })
            .expect("create native pseudo-terminal");
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_xana"));
        command.args(args);
        command.env("XANA_HOME", home);
        command.env("TERM", "xterm-256color");
        command.env("NO_COLOR", "1");
        command.env_remove("XANA_STORAGE_RECOVERY_KEY");
        command.cwd(workspace);
        let mut source = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let child = pair.slave.spawn_command(command).expect("spawn TUI");
        drop(pair.slave);
        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&output);
        let reader = thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            // Unix PTYs can report EIO when the last slave closes. Child exit,
            // readiness and observed draft output are the assertions below.
            while let Ok(count) = source.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let mut output = captured.lock().unwrap();
                let retained = count.min(OUTPUT_LIMIT.saturating_sub(output.len()));
                output.extend_from_slice(&buffer[..retained]);
            }
        });
        Self {
            child,
            writer: Some(writer),
            master: Some(pair.master),
            output,
            reader: Some(reader),
            terminal_reply_offset: 0,
            parsed_output_offset: 0,
            screen: vt100::Parser::new(35, 120, 0),
        }
    }

    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    fn wait_for(&mut self, marker: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let (text, cursor_query) = {
                let output = self.output.lock().unwrap();
                assert!(output.len() < OUTPUT_LIMIT, "TUI exceeded capture limit");
                self.screen.process(&output[self.parsed_output_offset..]);
                self.parsed_output_offset = output.len();
                let cursor_query = output[self.terminal_reply_offset..]
                    .windows(4)
                    .position(|bytes| bytes == b"\x1b[6n");
                (String::from_utf8_lossy(&output).into_owned(), cursor_query)
            };
            // ConPTY asks its terminal host for the initial cursor position.
            // This harness is that host; do not let startup wait for a human.
            if let Some(offset) = cursor_query {
                self.terminal_reply_offset += offset + 4;
                self.send(b"\x1b[1;1R");
            }
            let tail = &text[text.floor_char_boundary(text.len().saturating_sub(4096))..];
            assert!(!text.contains("overflowed its stack"), "{tail}");
            // Ratatui and ConPTY emit cursor-addressed diffs, not complete text
            // labels. Assert the current screen, not stale/fragmented raw bytes.
            let screen = self.screen.screen().contents();
            if screen.contains(marker) {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("TUI exited before {marker:?}: {status}\n{screen}\n{tail}");
            }
            assert!(
                Instant::now() < deadline,
                "TUI missing {marker:?}\n{screen}\nRaw tail:\n{tail}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let writer = self.writer.as_mut().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }

    fn quit(&mut self) {
        self.send(b"\x11"); // Ctrl+Q, through Crossterm and the runtime owner.
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "TUI exit: {status}\n{}",
                    self.transcript()
                );
                break;
            }
            assert!(Instant::now() < deadline, "TUI did not shut down");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for TuiProcess {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.writer.take();
        // Close the PTY while the reader still drains it (required by ConPTY).
        self.master.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn empty_native_tui_accepts_input_and_quits_on_the_production_stack() {
    // Include the ordinary dispatcher and the explicit new-session route.
    for args in [&["--tui"][..], &["session", "new"][..]] {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        // An unaccepted local socket proves startup/draft input need no model.
        let provider = TcpListener::bind("127.0.0.1:0").unwrap();
        provider.set_nonblocking(true).unwrap();
        let initialized = Command::new(env!("CARGO_BIN_EXE_xana"))
            .env("XANA_HOME", &home)
            .env_remove("XANA_STORAGE_RECOVERY_KEY")
            .current_dir(directory.path())
            .args([
                "init",
                "--non-interactive",
                "--kind",
                "ollama",
                "--provider-name",
                "test",
                "--base-url",
                &format!("http://{}/v1", provider.local_addr().unwrap()),
                "--model",
                "test-model",
                "--permission-mode",
                "deny",
            ])
            .output()
            .unwrap();
        assert!(initialized.status.success(), "{initialized:?}");
        let mut tui = TuiProcess::start(&home, directory.path(), args);
        // Wait for the attached Conversation, not the initial starting screen.
        tui.wait_for("[idle]");
        // No Enter: this originally crashed even with empty history and no Run.
        tui.send(b"a");
        // Collapsing the expanded header also proves the real input was handled.
        tui.wait_for("[/header]");
        let projects_path = home.join("data/interoperable/projects.json");
        let projects_before = std::fs::read(&projects_path).unwrap();
        tui.send(b"\x7f"); // Remove the draft character before the command.
        // Deliberate individual keys avoid exercising paste confirmation here.
        for byte in b"/settings" {
            thread::sleep(Duration::from_millis(60));
            tui.send(&[*byte]);
        }
        thread::sleep(Duration::from_millis(60));
        tui.send(b"\r");
        tui.wait_for("SETTINGS");
        tui.send(b"\x1b"); // Escape without edits returns to the same Conversation.
        tui.wait_for("[idle]");
        assert_eq!(std::fs::read(&projects_path).unwrap(), projects_before);
        tui.quit();
        assert!(!tui.transcript().contains("overflowed its stack"));
        assert_eq!(
            provider.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn terminal_screen_handles_fragmented_redraws_without_stale_readiness() {
    let mut screen = vt100::Parser::new(35, 120, 0);
    let redraw = b"\x1b[1;1H[/he\x1b[1;5Hader]";
    assert!(!String::from_utf8_lossy(redraw).contains("[/header]"));
    for byte in redraw {
        screen.process(&[*byte]);
    }
    assert!(screen.screen().contents().contains("[/header]"));
    screen.process(b"\x1b[2J\x1b[HSETTINGS");
    assert!(screen.screen().contents().contains("SETTINGS"));
    assert!(!screen.screen().contents().contains("[/header]"));
}
