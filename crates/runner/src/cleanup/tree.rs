use std::ffi::{CStr, OsStr, OsString};
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

use rustix::fs::{AtFlags, Dir, Mode, OFlags};
use rustix::io::Errno;
use rustix::path::Arg;

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

pub(super) fn open_directory<P: Arg + Copy>(
    parent: BorrowedFd<'_>,
    name: P,
) -> Result<OwnedFd, Errno> {
    match rustix::fs::openat(parent, name, directory_flags(), Mode::empty()) {
        Err(Errno::ACCESS) => {
            if permit(parent, name).is_err() {
                return Err(Errno::ACCESS);
            }
            rustix::fs::openat(parent, name, directory_flags(), Mode::empty())
        }
        other => other,
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn permit<P: Arg + Copy>(parent: BorrowedFd<'_>, name: P) -> Result<(), Errno> {
    use std::os::fd::AsRawFd;

    let held = rustix::fs::openat(
        parent,
        name,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let stat = rustix::fs::fstat(&held)?;
    rustix::fs::chmod(
        format!("/proc/self/fd/{}", held.as_raw_fd()),
        Mode::from_raw_mode(stat.st_mode | 0o700),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn permit<P: Arg + Copy>(parent: BorrowedFd<'_>, name: P) -> Result<(), Errno> {
    use rustix::fs::FileType;

    let stat = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(Errno::NOTDIR);
    }
    rustix::fs::chmodat(
        parent,
        name,
        Mode::from_raw_mode(stat.st_mode | 0o700),
        AtFlags::SYMLINK_NOFOLLOW,
    )
}

fn relax(directory: BorrowedFd<'_>) {
    if let Ok(stat) = rustix::fs::fstat(directory)
        && (stat.st_mode & 0o700) != 0o700
    {
        let _ = rustix::fs::fchmod(directory, Mode::from_raw_mode(stat.st_mode | 0o700));
    }
}

pub(super) fn remove_entries(
    directory: OwnedFd,
    depth: usize,
) -> io::Result<Vec<(OsString, io::Error)>> {
    let mut entries = Dir::new(directory)?;
    let mut failed = Vec::new();
    while let Some(entry) = entries.read() {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if let Err(error) = remove_entry(entries.fd()?, name, 1, depth) {
            failed.push((OsStr::from_bytes(name.to_bytes()).to_owned(), error));
        }
    }
    Ok(failed)
}

fn remove_entry(parent: BorrowedFd<'_>, name: &CStr, level: usize, depth: usize) -> io::Result<()> {
    let first = match rustix::fs::unlinkat(parent, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => return Ok(()),
        Err(errno @ (Errno::ISDIR | Errno::PERM)) => errno,
        Err(other) => return Err(other.into()),
    };
    if level > depth {
        return Err(io::Error::other(format!(
            "it is nested deeper than {depth} levels"
        )));
    }
    let opened = match open_directory(parent, name) {
        Ok(opened) => opened,
        Err(Errno::NOENT) => return Ok(()),
        Err(Errno::NOTDIR | Errno::LOOP) => return Err(first.into()),
        Err(other) => return Err(other.into()),
    };
    let mut entries = Dir::new(opened)?;
    relax(entries.fd()?);
    while let Some(entry) = entries.read() {
        let entry = entry?;
        let child = entry.file_name();
        if matches!(child.to_bytes(), b"." | b"..") {
            continue;
        }
        remove_entry(entries.fd()?, child, level + 1, depth)?;
    }
    drop(entries);
    match rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(other) => Err(other.into()),
    }
}
