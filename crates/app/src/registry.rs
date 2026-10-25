//! Text values under `HKEY_CURRENT_USER`, on Windows.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Sets `name` in `key`, creating the key if need be.
pub fn set(key: &str, name: &str, value: &str) -> std::io::Result<()> {
    let (key, name, value) = (wide(key), wide(name), wide(value));
    // SAFETY: NUL-terminated strings; the size is the value's, NUL included.
    let error = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_SZ, value.as_ptr().cast(), (value.len() * 2) as u32)
    };
    check(error)
}

pub fn get(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut size = 0u32;
    // SAFETY: NUL-terminated strings; no buffer, so only the size is written.
    let error = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
    };
    if error != ERROR_SUCCESS {
        return None;
    }
    let mut value = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: the buffer holds `size` bytes, which the call doesn't exceed.
    let error = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), value.as_mut_ptr().cast(), &mut size)
    };
    if error != ERROR_SUCCESS {
        return None;
    }
    let end = value.iter().position(|&unit| unit == 0).unwrap_or(value.len());
    Some(String::from_utf16_lossy(&value[..end]))
}

pub fn delete(key: &str, name: &str) -> std::io::Result<()> {
    let (key, name) = (wide(key), wide(name));
    // SAFETY: NUL-terminated strings.
    let error = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    check(error)
}

fn check(error: u32) -> std::io::Result<()> {
    if error == ERROR_SUCCESS { Ok(()) } else { Err(std::io::Error::from_raw_os_error(error as i32)) }
}
