//! A detached Windows owner must not retain its launcher's pipes or file leases.
//!
//! Rust 1.97's `Command` still inherits all inheritable handles; its opt-out is
//! unstable. Null stdio and CREATE_NO_WINDOW alone therefore do not detach pipe
//! lifetime. Keep this small native seam until a stable inheritance opt-out can
//! replace it. Environment/cwd remain inherited, but no handles cross the seam.

use anyhow::{Context, Result, ensure};
use std::{
    os::windows::{ffi::OsStrExt, io::FromRawHandle, io::OwnedHandle},
    path::Path,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
};

pub(super) fn detach(executable: &Path) -> Result<()> {
    // Windows has no Unix-style zombie requirement. Dropping our handle closes
    // observation ownership, not the independently running child's process.
    drop(launch(executable, &["autonomy", "host", "run"])?);
    Ok(())
}

fn command_line(executable: &Path, arguments: &[&str]) -> Result<(Vec<u16>, Vec<u16>)> {
    ensure!(
        executable.is_absolute(),
        "detached executable must be absolute"
    );
    let mut program: Vec<u16> = executable.as_os_str().encode_wide().collect();
    ensure!(
        !program.contains(&0) && !program.contains(&(b'"' as u16)),
        "detached executable contains an invalid Windows path character"
    );
    let mut command = Vec::with_capacity(program.len() + 64);
    command.push(b'"' as u16);
    command.extend_from_slice(&program);
    command.push(b'"' as u16);
    for argument in arguments {
        // This is not a general process API. Only these internally supplied
        // ASCII command words/test selectors are accepted; no shell is used.
        ensure!(
            !argument.is_empty()
                && argument
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_:".contains(&byte)),
            "detached command contains an unsupported internal argument"
        );
        command.push(b' ' as u16);
        command.extend(argument.encode_utf16());
    }
    ensure!(
        command.len() < 32767,
        "detached command exceeds Windows bounds"
    );
    command.push(0);
    program.push(0);
    Ok((program, command))
}

fn launch(executable: &Path, arguments: &[&str]) -> Result<OwnedHandle> {
    let (program, mut command) = command_line(executable, arguments)?;
    // SAFETY: Zero is the documented default for optional STARTUPINFO fields;
    // cb records the full initialized structure size. No stdio handles are set.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // SAFETY: CreateProcessW initializes both handles only on success.
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: program/command are live NUL-terminated UTF-16 buffers, and command
    // is writable as Win32 requires. The exact executable path prevents search
    // ambiguity. Null security/environment/cwd select default caller ownership
    // and inherited environment/cwd. FALSE forbids every inherited handle;
    // CREATE_NO_WINDOW supplies no visible console. Pointers stay live through
    // this synchronous call; no global handle flags or environment are mutated.
    let created = unsafe {
        CreateProcessW(
            program.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut information,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error()).context("could not launch detached host");
    }
    // SAFETY: Successful CreateProcessW transferred these two valid, distinct
    // handles to this caller. Each enters exactly one OwnedHandle and is closed
    // exactly once; closing either does not terminate the process.
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    drop(unsafe { OwnedHandle::from_raw_handle(information.hThread) });
    Ok(process)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::windows::io::AsRawHandle, sync::mpsc, time::Duration};
    use windows_sys::Win32::{
        Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_TIMEOUT},
        Security::SECURITY_ATTRIBUTES,
        System::{
            Pipes::CreatePipe,
            Threading::{TerminateProcess, WaitForSingleObject},
        },
    };

    #[test]
    fn executable_is_quoted_without_shell_or_argument_injection() {
        let (_, encoded) = command_line(
            Path::new("C:/fixture space/日本語/xana.exe"),
            &["autonomy", "host", "run"],
        )
        .unwrap();
        assert_eq!(
            String::from_utf16(&encoded[..encoded.len() - 1]).unwrap(),
            "\"C:/fixture space/日本語/xana.exe\" autonomy host run"
        );
        assert!(command_line(Path::new("xana.exe"), &["host"]).is_err());
        assert!(command_line(Path::new("C:/xana.exe"), &["run & echo unwanted"]).is_err());
        assert!(command_line(Path::new("C:/bad\"/xana.exe"), &["run"]).is_err());
    }

    #[test]
    fn detached_windows_child_does_not_keep_redirected_pipes_open() {
        // Create the inheritable pipe in an isolated test process so unrelated
        // concurrently spawning tests cannot accidentally inherit its writer.
        let probe = test_selector("windows_inheritance_probe");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &probe, "--ignored", "--nocapture"])
            .output()
            .unwrap();
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("running 1 test")
                && String::from_utf8_lossy(&output.stdout).contains("isolated-pipe-eof-verified"),
            "isolated pipe probe failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn test_selector(name: &str) -> String {
        let (_, module) = module_path!().split_once("::").unwrap();
        format!("{module}::{name}")
    }

    #[test]
    #[ignore = "subprocess body run by detached_windows_child_does_not_keep_redirected_pipes_open"]
    fn windows_inheritance_probe() {
        let mut read = std::ptr::null_mut();
        let mut write = std::ptr::null_mut();
        let security = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        // SAFETY: output pointers and security attributes are valid for this
        // call; successful handles are immediately given unique RAII owners.
        assert_ne!(
            unsafe { CreatePipe(&mut read, &mut write, &security, 0) },
            0
        );
        let read = unsafe { OwnedHandle::from_raw_handle(read) };
        let write = unsafe { OwnedHandle::from_raw_handle(write) };
        // SAFETY: read is a live uniquely owned pipe handle. Only the synthetic
        // writer deliberately remains inheritable for the child-spawn probe.
        assert_ne!(
            unsafe { SetHandleInformation(read.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) },
            0
        );
        let sleeper = test_selector("windows_detached_sleeper");
        let child = launch(
            &std::env::current_exe().unwrap(),
            &["--exact", &sleeper, "--ignored"],
        )
        .unwrap();
        drop(write);
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut file = std::fs::File::from(read);
            let _ = send.send(file.read(&mut [0u8; 1]));
        });
        let eof = receive.recv_timeout(Duration::from_secs(2));
        // SAFETY: child is this probe's exact live process handle, not a PID
        // lookup. Always terminate/wait before asserting so failures don't leak.
        let alive = unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } == WAIT_TIMEOUT;
        unsafe {
            TerminateProcess(child.as_raw_handle(), 0);
            WaitForSingleObject(child.as_raw_handle(), 5000);
        }
        reader.join().unwrap();
        assert!(
            alive,
            "probe child must still be alive when pipe EOF arrives"
        );
        assert!(
            matches!(eof, Ok(Ok(0))),
            "detached child inherited the launcher's writer: {eof:?}"
        );
        println!("isolated-pipe-eof-verified");
    }

    #[test]
    #[ignore = "bounded test-owned child, terminated by windows_inheritance_probe"]
    fn windows_detached_sleeper() {
        std::thread::sleep(Duration::from_secs(20));
    }
}
