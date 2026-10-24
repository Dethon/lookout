//! Where refresh tokens are kept. On Windows they are encrypted for the
//! user with DPAPI in a file, since Credential Manager can't hold a long
//! refresh token. Elsewhere they go in the system keyring, or a file only
//! the user can read when there is none.

use std::path::PathBuf;

#[cfg(not(windows))]
const SERVICE: &str = "lookout";

fn file_path(account: &str) -> crate::Result<PathBuf> {
    let safe: String = account
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    Ok(crate::config::data_dir()?.join("tokens").join(safe))
}

pub fn load(account: &str) -> Option<String> {
    #[cfg(not(windows))]
    if let Ok(entry) = keyring::Entry::new(SERVICE, account)
        && let Ok(secret) = entry.get_password()
    {
        return Some(secret);
    }
    let bytes = std::fs::read(file_path(account).ok()?).ok()?;
    #[cfg(windows)]
    let bytes = dpapi::unprotect(&bytes)?;
    String::from_utf8(bytes).ok()
}

pub fn store(account: &str, secret: &str) -> crate::Result<()> {
    #[cfg(not(windows))]
    if let Ok(entry) = keyring::Entry::new(SERVICE, account)
        && entry.set_password(secret).is_ok()
    {
        return Ok(());
    }
    let path = file_path(account)?;
    std::fs::create_dir_all(path.parent().expect("a token file has a folder"))?;
    #[cfg(windows)]
    let bytes = dpapi::protect(secret.as_bytes())?;
    #[cfg(not(windows))]
    let bytes = secret.as_bytes().to_vec();
    write_private(&path, &bytes)?;
    Ok(())
}

pub fn delete(account: &str) {
    #[cfg(not(windows))]
    if let Ok(entry) = keyring::Entry::new(SERVICE, account) {
        _ = entry.delete_credential();
    }
    if let Ok(path) = file_path(account) {
        _ = std::fs::remove_file(path);
    }
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut u8,
        }
    }

    /// Copies DPAPI's output and frees it.
    ///
    /// SAFETY: `out` must have been filled by a successful DPAPI call.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(out.pbData.cast()) };
        bytes
    }

    pub fn protect(bytes: &[u8]) -> crate::Result<Vec<u8>> {
        let input = blob(bytes);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: the blobs point at live buffers for the call's duration.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(crate::Error::Secret(std::io::Error::last_os_error().to_string()));
        }
        // SAFETY: the call succeeded.
        Ok(unsafe { take(out) })
    }

    pub fn unprotect(bytes: &[u8]) -> Option<Vec<u8>> {
        let input = blob(bytes);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: as in `protect`.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        // SAFETY: the call succeeded.
        (ok != 0).then(|| unsafe { take(out) })
    }
}
