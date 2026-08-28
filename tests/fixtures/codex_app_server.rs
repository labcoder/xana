//! Inert, scripted JSONL peer for Codex adapter integration tests.
//!
//! No vendor executable, credentials, network access, or model calls are used.

use std::{
    env, fs,
    io::{self, BufRead, Write},
    path::PathBuf,
};

fn main() {
    let home = PathBuf::from(env::var_os("CODEX_HOME").expect("fixture home"));
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--version"] {
        let version = fs::read_to_string(home.join("version"))
            .unwrap_or_else(|_| "codex-cli 0.147.0".to_owned());
        println!("{version}");
        return;
    }
    assert_eq!(arguments, ["app-server", "--stdio"]);

    let script = fs::read_to_string(home.join("script")).expect("fixture script");
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut frame = String::new();
    for instruction in script.lines() {
        if let Some(expected) = instruction.strip_prefix("READ ") {
            frame.clear();
            assert_ne!(input.read_line(&mut frame).expect("read frame"), 0);
            assert!(
                frame.contains(expected),
                "expected {expected:?}, got {frame:?}"
            );
        } else if let Some(expected) = instruction.strip_prefix("HAS ") {
            assert!(
                frame.contains(expected),
                "expected {expected:?}, got {frame:?}"
            );
        } else if let Some(unexpected) = instruction.strip_prefix("LACKS ") {
            assert!(
                !frame.contains(unexpected),
                "unexpected {unexpected:?} in {frame:?}"
            );
        } else if let Some(response) = instruction.strip_prefix("SEND ") {
            writeln!(output, "{response}").expect("write response");
            output.flush().expect("flush response");
        } else {
            panic!("unknown fixture instruction {instruction:?}");
        }
    }
    frame.clear();
    assert_eq!(
        input.read_line(&mut frame).expect("wait for shutdown"),
        0,
        "unexpected request after the scripted exchange: {frame:?}"
    );
    fs::write(home.join("completed"), "ok").expect("record checked exchange and EOF");
}
