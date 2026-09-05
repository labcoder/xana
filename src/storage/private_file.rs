//! Create-only independent secret exports with permissions set at creation.

use std::{fs::File, io, path::Path};

#[cfg(unix)]
pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(windows)]
pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    use std::{
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree},
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL},
    };
    let name = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    if name[..name.len() - 1].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secret path contains NUL",
        ));
    }
    // Protected DACL, file owner only. Do not inherit broad directory grants.
    let sddl = "D:P(A;;FA;;;OW)"
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
    let mut descriptor = ptr::null_mut();
    // SAFETY: both strings are NUL-terminated and live through these calls. The
    // descriptor is LocalFree-owned and is freed once after CreateFileW copies
    // it. A successful handle is transferred exactly once into File ownership.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let handle = CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        );
        let error = (handle == INVALID_HANDLE_VALUE).then(io::Error::last_os_error);
        LocalFree(descriptor);
        match error {
            Some(error) => Err(error),
            None => Ok(File::from_raw_handle(handle)),
        }
    }
}

#[cfg(not(any(windows, unix)))]
pub(crate) fn create_private_file(_: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private secret export is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn secret_export_is_create_only_and_owner_readable() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovery.txt");
        let mut file = super::create_private_file(&path).unwrap();
        file.write_all(b"fixture secret").unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"fixture secret");
        assert!(super::create_private_file(&path).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
