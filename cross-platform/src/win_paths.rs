//! Read Windows-owned paths through Win32, never by assuming C: or trusting
//! environment variables when choosing an elevated executable.
use anyhow::{ensure, Result};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf, ptr};
use windows_sys::{
    core::GUID,
    Win32::{
        System::{Com::CoTaskMemFree, SystemInformation::GetSystemDirectoryW},
        UI::Shell::SHGetKnownFolderPath,
    },
};

pub fn known_folder(id: &GUID) -> Result<PathBuf> {
    let mut value = ptr::null_mut();
    unsafe {
        let result = SHGetKnownFolderPath(id, 0, ptr::null_mut(), &mut value);
        ensure!(
            result >= 0 && !value.is_null(),
            "Cannot read Windows known folder: {result:#x}"
        );
        let mut len = 0;
        while *value.add(len) != 0 {
            len += 1;
        }
        let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(value, len)));
        CoTaskMemFree(value.cast());
        Ok(path)
    }
}
pub fn system_directory() -> Result<PathBuf> {
    let mut buf = vec![0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    ensure!(
        n > 0 && n < buf.len(),
        "Cannot read Windows system directory"
    );
    Ok(PathBuf::from(OsString::from_wide(&buf[..n])))
}
pub fn powershell() -> Result<PathBuf> {
    Ok(system_directory()?
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe"))
}
