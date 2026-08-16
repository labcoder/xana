use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, BufRead, Write},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn main() {
    match env::args().nth(1).as_deref().unwrap_or("serve") {
        "serve" => serve(),
        "sleep" => thread::sleep(Duration::from_secs(30)),
        "tree" => spawn_tree(),
        "stderr" => drain_stderr(),
        "malformed" => malformed(),
        mode => panic!("unknown fixture mode: {mode}"),
    }
}

fn spawn_tree() {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/S", "/C", "ping -n 30 127.0.0.1 >NUL"]);
        command
    } else {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        command
    };
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("fixture spawns descendant");
    let path = env::args().nth(2).expect("tree fixture requires pid path");
    fs::write(path, child.id().to_string()).expect("fixture writes descendant pid");
    thread::sleep(Duration::from_secs(30));
}

fn serve() {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("fixture reads request");
        let Some(id) = request_id(&line) else {
            continue;
        };
        let response = if line.contains(r#""method":"server/discover""#) {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"supportedVersions":["2026-07-28"],"capabilities":{{"tools":{{"listChanged":true}}}},"_meta":{{"io.modelcontextprotocol/serverInfo":{{"name":"fixture","version":"1"}}}}}}}}"#
            )
        } else if line.contains(r#""method":"fixture/slow""#) {
            continue;
        } else if line.contains(r#""method":"fixture/env""#) {
            let inherited_home = if cfg!(windows) {
                env::var_os("USERPROFILE")
            } else {
                env::var_os("HOME")
            };
            let ok = env::var_os("XANA_MCP_EXPLICIT").as_deref() == Some(OsStr::new("allowed"))
                && inherited_home.is_none();
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"resultType":"complete","ok":{ok}}}}}"#
            )
        } else {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"resultType":"complete","ok":true}}}}"#
            )
        };
        writeln!(stdout, "{response}").expect("fixture writes response");
        stdout.flush().expect("fixture flushes response");
    }
}

fn request_id(line: &str) -> Option<u64> {
    let (_, after_marker) = line.split_once(r#""id":"#)?;
    let digits = after_marker
        .trim_start()
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

fn drain_stderr() {
    let mut stderr = io::stderr().lock();
    stderr
        .write_all(&vec![b'x'; 100_000])
        .expect("fixture writes stderr");
    stderr.flush().expect("fixture flushes stderr");
    for line in io::stdin().lock().lines() {
        line.expect("fixture drains stdin");
    }
}

fn malformed() {
    println!("not-json");
    io::stdout().flush().expect("fixture flushes stdout");
    thread::sleep(Duration::from_secs(1));
}
