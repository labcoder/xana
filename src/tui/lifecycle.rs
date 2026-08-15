//! Idempotent ownership of process-global terminal modes.

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, Stdout};

trait TerminalControl {
    fn enable_raw(&mut self) -> io::Result<()>;
    fn enter_alternate(&mut self) -> io::Result<()>;
    fn enable_mouse(&mut self) -> io::Result<()>;
    fn enable_paste(&mut self) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn disable_paste(&mut self) -> io::Result<()>;
    fn disable_mouse(&mut self) -> io::Result<()>;
    fn leave_alternate(&mut self) -> io::Result<()>;
    fn disable_raw(&mut self) -> io::Result<()>;
}

struct CrosstermControl;

pub(super) fn restore_process_terminal_best_effort() {
    let _ = execute!(
        io::stdout(),
        Show,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

impl TerminalControl for CrosstermControl {
    fn enable_raw(&mut self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn enter_alternate(&mut self) -> io::Result<()> {
        execute!(io::stdout(), EnterAlternateScreen).map(|_| ())
    }

    fn enable_mouse(&mut self) -> io::Result<()> {
        execute!(io::stdout(), EnableMouseCapture).map(|_| ())
    }

    fn enable_paste(&mut self) -> io::Result<()> {
        execute!(io::stdout(), EnableBracketedPaste).map(|_| ())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(io::stdout(), Hide).map(|_| ())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(io::stdout(), Show).map(|_| ())
    }

    fn disable_paste(&mut self) -> io::Result<()> {
        execute!(io::stdout(), DisableBracketedPaste).map(|_| ())
    }

    fn disable_mouse(&mut self) -> io::Result<()> {
        execute!(io::stdout(), DisableMouseCapture).map(|_| ())
    }

    fn leave_alternate(&mut self) -> io::Result<()> {
        execute!(io::stdout(), LeaveAlternateScreen).map(|_| ())
    }

    fn disable_raw(&mut self) -> io::Result<()> {
        disable_raw_mode()
    }
}

#[derive(Default)]
struct AcquiredModes {
    raw: bool,
    alternate: bool,
    mouse: bool,
    paste: bool,
    cursor_hidden: bool,
    restored: bool,
}

struct TerminalLifecycle<C: TerminalControl> {
    control: C,
    modes: AcquiredModes,
}

impl<C: TerminalControl> TerminalLifecycle<C> {
    fn enter(control: C) -> io::Result<Self> {
        let mut owner = Self {
            control,
            modes: AcquiredModes::default(),
        };

        owner.modes.raw = true;
        owner.control.enable_raw()?;
        owner.modes.alternate = true;
        owner.control.enter_alternate()?;
        owner.modes.mouse = true;
        owner.control.enable_mouse()?;
        owner.modes.paste = true;
        owner.control.enable_paste()?;
        owner.modes.cursor_hidden = true;
        owner.control.hide_cursor()?;
        Ok(owner)
    }

    fn restore(&mut self) {
        if self.modes.restored {
            return;
        }
        self.modes.restored = true;
        if self.modes.cursor_hidden {
            let _ = self.control.show_cursor();
        }
        if self.modes.paste {
            let _ = self.control.disable_paste();
        }
        if self.modes.mouse {
            let _ = self.control.disable_mouse();
        }
        if self.modes.alternate {
            let _ = self.control.leave_alternate();
        }
        if self.modes.raw {
            let _ = self.control.disable_raw();
        }
    }
}

impl<C: TerminalControl> Drop for TerminalLifecycle<C> {
    fn drop(&mut self) {
        self.restore();
    }
}

pub(crate) struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    lifecycle: TerminalLifecycle<CrosstermControl>,
}

impl TerminalSession {
    pub(crate) fn enter() -> io::Result<Self> {
        let lifecycle = TerminalLifecycle::enter(CrosstermControl)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok(Self {
            terminal,
            lifecycle,
        })
    }

    pub(crate) fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    pub(crate) fn restore(&mut self) {
        let _ = self.terminal.show_cursor();
        self.lifecycle.restore();
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, Mutex},
    };

    #[derive(Clone)]
    struct FakeControl {
        log: Arc<Mutex<Vec<&'static str>>>,
        fail_at: Option<&'static str>,
    }

    impl FakeControl {
        fn step(&self, name: &'static str) -> io::Result<()> {
            self.log.lock().unwrap().push(name);
            if self.fail_at == Some(name) {
                Err(io::Error::other(format!("failed at {name}")))
            } else {
                Ok(())
            }
        }
    }

    impl TerminalControl for FakeControl {
        fn enable_raw(&mut self) -> io::Result<()> {
            self.step("raw+")
        }
        fn enter_alternate(&mut self) -> io::Result<()> {
            self.step("alt+")
        }
        fn enable_mouse(&mut self) -> io::Result<()> {
            self.step("mouse+")
        }
        fn enable_paste(&mut self) -> io::Result<()> {
            self.step("paste+")
        }
        fn hide_cursor(&mut self) -> io::Result<()> {
            self.step("cursor-")
        }
        fn show_cursor(&mut self) -> io::Result<()> {
            self.step("cursor+")
        }
        fn disable_paste(&mut self) -> io::Result<()> {
            self.step("paste-")
        }
        fn disable_mouse(&mut self) -> io::Result<()> {
            self.step("mouse-")
        }
        fn leave_alternate(&mut self) -> io::Result<()> {
            self.step("alt-")
        }
        fn disable_raw(&mut self) -> io::Result<()> {
            self.step("raw-")
        }
    }

    fn fake(fail_at: Option<&'static str>) -> (FakeControl, Arc<Mutex<Vec<&'static str>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        (
            FakeControl {
                log: Arc::clone(&log),
                fail_at,
            },
            log,
        )
    }

    #[test]
    fn every_partial_initialization_restores_only_acquired_modes() {
        for failed in ["raw+", "alt+", "mouse+", "paste+", "cursor-"] {
            let (control, log) = fake(Some(failed));
            assert!(TerminalLifecycle::enter(control).is_err());
            let log = log.lock().unwrap();
            assert_eq!(log.iter().filter(|step| **step == "raw-").count(), 1);
            if failed != "raw+" {
                assert_eq!(log.iter().filter(|step| **step == "alt-").count(), 1);
            }
        }
    }

    #[test]
    fn explicit_restore_drop_and_panic_cleanup_are_exactly_once() {
        let (control, log) = fake(None);
        {
            let mut lifecycle = TerminalLifecycle::enter(control).unwrap();
            lifecycle.restore();
            lifecycle.restore();
        }
        for cleanup in ["cursor+", "paste-", "mouse-", "alt-", "raw-"] {
            assert_eq!(
                log.lock()
                    .unwrap()
                    .iter()
                    .filter(|step| **step == cleanup)
                    .count(),
                1
            );
        }

        let (control, panic_log) = fake(None);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _lifecycle = TerminalLifecycle::enter(control).unwrap();
            panic!("test unwind");
        }));
        assert!(result.is_err());
        for cleanup in ["cursor+", "paste-", "mouse-", "alt-", "raw-"] {
            assert_eq!(
                panic_log
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|step| **step == cleanup)
                    .count(),
                1
            );
        }
    }
}
