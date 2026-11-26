//! Dirfd-relative scratch note writes. Keep the directory open through the atomic rename.

use std::ffi::{c_char, c_int, CStr, CString, OsStr};
use std::fs::{File, Permissions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

#[cfg(any(target_os = "linux", target_os = "netbsd", target_os = "openbsd"))]
type Mode = u32;
#[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "dragonfly"))]
type Mode = u16;

// openat flags differ between the supported Unix targets.
#[cfg(target_os = "linux")]
const CREATE_EXCLUSIVE: c_int = 0o100 | 0o200;
#[cfg(not(target_os = "linux"))]
const CREATE_EXCLUSIVE: c_int = 0x0200 | 0x0800;
#[cfg(all(target_os = "linux", any(target_arch = "arm", target_arch = "aarch64")))]
const DIRECTORY_NOFOLLOW: c_int = 0x4000 | 0x8000;
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "arm", target_arch = "aarch64"))
))]
const DIRECTORY_NOFOLLOW: c_int = 0x10000 | 0x20000;
#[cfg(target_os = "linux")]
const CLOEXEC: c_int = 0x80000;
#[cfg(target_os = "macos")]
const DIRECTORY_NOFOLLOW: c_int = 0x00100000 | 0x0100;
#[cfg(target_os = "macos")]
const CLOEXEC: c_int = 0x01000000;
#[cfg(target_os = "freebsd")]
const DIRECTORY_NOFOLLOW: c_int = 0x00020000 | 0x0100;
#[cfg(target_os = "freebsd")]
const CLOEXEC: c_int = 0x00100000;
#[cfg(target_os = "openbsd")]
const DIRECTORY_NOFOLLOW: c_int = 0x20000 | 0x0100;
#[cfg(target_os = "openbsd")]
const CLOEXEC: c_int = 0x10000;
#[cfg(target_os = "netbsd")]
const DIRECTORY_NOFOLLOW: c_int = 0x200000 | 0x0100;
#[cfg(target_os = "netbsd")]
const CLOEXEC: c_int = 0x400000;
#[cfg(target_os = "dragonfly")]
const DIRECTORY_NOFOLLOW: c_int = 0x08000000 | 0x0100;
#[cfg(target_os = "dragonfly")]
const CLOEXEC: c_int = 0x00020000;

extern "C" {
    fn openat(dir: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn mkdirat(dir: c_int, path: *const c_char, mode: Mode) -> c_int;
    fn readlinkat(dir: c_int, path: *const c_char, buf: *mut c_char, len: usize) -> isize;
    fn renameat(from_dir: c_int, from: *const c_char, to_dir: c_int, to: *const c_char) -> c_int;
    fn unlinkat(dir: c_int, path: *const c_char, flags: c_int) -> c_int;
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(io::Error::other)
}

fn open_at(dir: &File, name: &CStr, flags: c_int, mode: c_int) -> io::Result<File> {
    // The name is NUL-terminated and dir stays alive through the call.
    let fd = unsafe { openat(dir.as_raw_fd(), name.as_ptr(), flags | CLOEXEC, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // openat returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_dir_at(dir: &File, name: &CStr, create: bool) -> io::Result<File> {
    match open_at(dir, name, DIRECTORY_NOFOLLOW, 0) {
        Err(err) if create && err.kind() == io::ErrorKind::NotFound => {
            // A concurrent creator is fine. Reopen with O_NOFOLLOW in either case.
            if unsafe { mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::AlreadyExists {
                    return Err(err);
                }
            }
            open_at(dir, name, DIRECTORY_NOFOLLOW, 0)
        }
        result => result,
    }
}

fn open_data_root(path: &Path) -> io::Result<File> {
    // macOS exposes these system directories through aliases at the filesystem root.
    #[cfg(target_os = "macos")]
    let system_path = ["/tmp", "/var"].into_iter().find_map(|alias| {
        path.strip_prefix(alias)
            .ok()
            .map(|rest| Path::new("/private").join(&alias[1..]).join(rest))
    });
    #[cfg(target_os = "macos")]
    let path = system_path.as_deref().unwrap_or(path);

    let mut dir = File::open(if path.is_absolute() { "/" } else { "." })?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => dir = open_dir_at(&dir, &c_name(name)?, false)?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid data root",
                ))
            }
        }
    }
    Ok(dir)
}

pub(super) fn save(data_root: &Path, cid: &str, name: &str, body: &str) -> io::Result<PathBuf> {
    let root = open_data_root(data_root)?;
    let brain = open_dir_at(&root, c"brain", true)?;
    let conversation = open_dir_at(&brain, &c_name(OsStr::new(cid))?, true)?;
    let scratch = open_dir_at(&conversation, c"scratch", true)?;
    let destination = c_name(OsStr::new(name))?;
    let fd = scratch.as_raw_fd();

    // Keep rejecting existing symlinks. A later swap is safe because renameat replaces
    // the destination directory entry without following it.
    let mut byte = 0;
    if unsafe { readlinkat(fd, destination.as_ptr(), &mut byte, 1) } >= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "note is a symlink",
        ));
    }
    let error = io::Error::last_os_error();
    if !matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
    ) {
        return Err(error);
    }

    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|err| io::Error::other(err.to_string()))?;
    let temp_name = CString::new(format!(".slice-{:032x}.tmp", u128::from_ne_bytes(nonce)))
        .expect("hexadecimal name has no NUL");
    const WRITE_ONLY: c_int = 1;
    let mut file = open_at(&scratch, &temp_name, WRITE_ONLY | CREATE_EXCLUSIVE, 0o600)?;
    let result = (|| {
        file.set_permissions(Permissions::from_mode(0o600))?;
        file.write_all(body.as_bytes())?;
        if unsafe { renameat(fd, temp_name.as_ptr(), fd, destination.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(data_root.join("brain").join(cid).join("scratch").join(name))
    })();
    if result.is_err() {
        unsafe { unlinkat(fd, temp_name.as_ptr(), 0) };
    }
    result
}
