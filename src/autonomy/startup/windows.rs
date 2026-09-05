//! HKCU Run only: no machine-wide service, elevation or shell command.
use super::Registration;
use anyhow::{Result, ensure};
use std::{io, ptr};
use windows_sys::Win32::{Foundation::ERROR_FILE_NOT_FOUND, System::Registry::*};

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

pub(super) fn change(registration: &Registration, enable: bool) -> Result<()> {
    let command = registration
        .arguments
        .iter()
        .map(|argument| quote(argument))
        .collect::<Vec<_>>()
        .join(" ");
    // Microsoft's documented Run value limit is 260 characters.
    ensure!(
        command.encode_utf16().count() <= 260,
        "startup command exceeds the Windows Run limit; use a shorter installed path/home"
    );
    let name = wide(&registration.name);
    let expected = wide(&command);
    let path = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let mut raw = ptr::null_mut();
    // SAFETY: all strings are terminated, buffers stay live, the returned key
    // has one RAII owner, and registry sizes are bounded before reading.
    let opened = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            ptr::null(),
            &mut raw,
            ptr::null_mut(),
        )
    };
    if opened != 0 {
        return Err(io::Error::from_raw_os_error(opened as i32).into());
    }
    let key = Key(raw);
    let mut kind = 0;
    let mut size = 0;
    let queried = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            ptr::null(),
            &mut kind,
            ptr::null_mut(),
            &mut size,
        )
    };
    if queried == 0 {
        ensure!(
            kind == REG_SZ && size <= 522 && size % 2 == 0,
            "existing startup entry is not Xana's bounded string"
        );
        let mut value = vec![0u16; size as usize / 2];
        let read = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                value.as_mut_ptr().cast(),
                &mut size,
            )
        };
        ensure!(
            read == 0 && kind == REG_SZ && value == expected,
            "startup entry changed or belongs to a different command; it was not replaced or removed"
        );
        if enable {
            return Ok(());
        }
    } else if queried == ERROR_FILE_NOT_FOUND {
        if !enable {
            return Ok(());
        }
    } else {
        return Err(io::Error::from_raw_os_error(queried as i32).into());
    }
    let result = unsafe {
        if enable {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_SZ,
                expected.as_ptr().cast(),
                u32::try_from(expected.len() * 2)?,
            )
        } else {
            RegDeleteValueW(key.0, name.as_ptr())
        }
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32).into());
    }
    Ok(())
}

fn quote(argument: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for ch in argument.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            if ch == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        result.push(ch);
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_arguments_are_quoted_without_a_shell() {
        assert_eq!(
            super::quote("C:\\Program Files\\Xana\\xana.exe"),
            "\"C:\\Program Files\\Xana\\xana.exe\""
        );
        assert_eq!(super::quote("C:\\task home\\"), "\"C:\\task home\\\\\"");
        assert_eq!(super::quote("a\"b"), "\"a\\\"b\"");
    }
}
