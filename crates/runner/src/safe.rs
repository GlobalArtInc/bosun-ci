use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

pub const FILE_LIMIT: u64 = 8 * 1024 * 1024;

pub fn open(path: &Path) -> io::Result<File> {
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| {
        link(
            errno,
            "it is a symbolic link, which the agent does not follow",
        )
    })?;
    regular(descriptor)
}

pub(crate) fn open_below(root: &Path, relative: &Path) -> io::Result<File> {
    let mut names = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "it leaves the directory it is read from",
                ));
            }
        }
    }
    let Some((last, parents)) = names.split_last() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "it names no file",
        ));
    };
    let mut directory = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    for name in parents {
        directory = rustix::fs::openat(
            &directory,
            *name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|errno| {
            if errno == Errno::LOOP || errno == Errno::NOTDIR {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a directory on the way to it is a symbolic link or not a directory, which \
                     the agent does not follow",
                )
            } else {
                io::Error::from(errno)
            }
        })?;
    }
    let descriptor = rustix::fs::openat(
        &directory,
        *last,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| {
        link(
            errno,
            "it is a symbolic link, which the agent does not follow",
        )
    })?;
    regular(descriptor)
}

fn link(errno: Errno, message: &'static str) -> io::Error {
    if errno == Errno::LOOP {
        io::Error::new(io::ErrorKind::InvalidInput, message)
    } else {
        io::Error::from(errno)
    }
}

fn regular(descriptor: OwnedFd) -> io::Result<File> {
    let stat = rustix::fs::fstat(&descriptor)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "it is not a regular file",
        ));
    }
    Ok(File::from(descriptor))
}

pub fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    take(open(path)?, limit)
}

pub(crate) fn read_below(root: &Path, relative: &Path, limit: u64) -> io::Result<Vec<u8>> {
    take(open_below(root, relative)?, limit)
}

fn take(file: File, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(format!(
            "it is larger than {} MiB",
            limit / (1024 * 1024)
        )));
    }
    Ok(bytes)
}

pub fn create_file(path: &Path, mode: u32) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(found) if found.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

pub fn create(path: &Path, mode: u32, content: &[u8]) -> io::Result<()> {
    create_file(path, mode)?.write_all(content)
}

pub fn directory_below(root: &Path, relative: &Path) -> io::Result<()> {
    let mut directory = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    for name in names(relative)? {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        directory = match rustix::fs::openat(&directory, name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(Errno::NOENT) => {
                rustix::fs::mkdirat(&directory, name, Mode::from_raw_mode(0o777))?;
                rustix::fs::openat(&directory, name, flags, Mode::empty())?
            }
            Err(Errno::LOOP | Errno::NOTDIR) => {
                rustix::fs::unlinkat(&directory, name, AtFlags::empty())?;
                rustix::fs::mkdirat(&directory, name, Mode::from_raw_mode(0o777))?;
                rustix::fs::openat(&directory, name, flags, Mode::empty())?
            }
            Err(other) => return Err(other.into()),
        };
    }
    Ok(())
}

pub fn is_directory_below(root: &Path, relative: &Path) -> io::Result<bool> {
    let mut directory = rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    for name in names(relative)? {
        directory = match rustix::fs::openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(next) => next,
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => return Ok(false),
            Err(other) => return Err(other.into()),
        };
    }
    Ok(true)
}

fn names(relative: &Path) -> io::Result<Vec<&std::ffi::OsStr>> {
    let mut names = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "it leaves the directory it lies in",
                ));
            }
        }
    }
    Ok(names)
}

pub(crate) fn directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(found) if found.is_dir() => Ok(()),
        Ok(_) => {
            fs::remove_file(path)?;
            fs::create_dir_all(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(crate) fn promptly<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the work ended within 30 seconds")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;

    use super::{create, directory, open, open_below, read};

    #[test]
    fn a_file_is_read_only_when_it_is_a_regular_file_of_a_size_the_agent_accepts() {
        let directory = tempfile::tempdir().expect("a directory");
        let secret = directory.path().join("secret");
        fs::write(&secret, "the credential of the agent").expect("a file");
        let file = directory.path().join("set_output");
        fs::write(&file, "a=b\n").expect("a file");
        assert_eq!(read(&file, 16).expect("a read"), b"a=b\n");
        assert!(
            read(&file, 2)
                .expect_err("too large")
                .to_string()
                .contains("larger")
        );
        let link = directory.path().join("link");
        symlink(&secret, &link).expect("a link");
        let refused = open(&link).expect_err("a link was followed");
        assert!(refused.to_string().contains("symbolic link"), "{refused}");
        let fifo = directory.path().join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("mkfifo")
                .success()
        );
        let refused = open(&fifo).expect_err("a pipe was opened");
        assert!(
            refused.to_string().contains("not a regular file"),
            "{refused}"
        );
        assert_eq!(
            read(&directory.path().join("absent"), 16)
                .expect_err("absent")
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn a_file_is_created_over_a_link_a_file_or_a_directory_without_writing_through_it() {
        let directory = tempfile::tempdir().expect("a directory");
        let target = directory.path().join("target");
        fs::write(&target, "kept").expect("a file");
        let path = directory.path().join("step-1.sh");
        symlink(&target, &path).expect("a link");
        create(&path, 0o600, b"script").expect("a creation");
        assert_eq!(fs::read_to_string(&target).expect("read"), "kept");
        assert_eq!(fs::read_to_string(&path).expect("read"), "script");
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
        create(&path, 0o600, b"again").expect("a creation over a file");
        fs::remove_file(&path).expect("removed");
        fs::create_dir(&path).expect("a directory");
        fs::write(path.join("inside"), "x").expect("a file");
        create(&path, 0o600, b"over a directory").expect("a creation over a directory");
        assert_eq!(fs::read_to_string(&path).expect("read"), "over a directory");
    }

    #[test]
    fn a_link_in_the_place_of_a_directory_is_replaced_by_a_directory() {
        let directory = tempfile::tempdir().expect("a directory");
        let outside = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("_runner_file_commands");
        symlink(outside.path(), &path).expect("a link");
        directory_of(&path);
        assert!(fs::symlink_metadata(&path).expect("metadata").is_dir());
        assert_eq!(fs::read_dir(outside.path()).expect("listed").count(), 0);
    }

    fn directory_of(path: &std::path::Path) {
        directory(path).expect("a directory");
    }

    #[test]
    fn a_file_below_a_root_is_opened_only_when_no_link_lies_on_its_way() {
        let directory = tempfile::tempdir().expect("a directory");
        let root = fs::canonicalize(directory.path()).expect("a path");
        let outside = tempfile::tempdir().expect("a directory");
        fs::write(outside.path().join("secret"), "x").expect("a file");
        fs::create_dir_all(root.join("real/deeper")).expect("directories");
        fs::write(root.join("real/deeper/file"), "x").expect("a file");
        symlink(outside.path(), root.join("linked")).expect("a link");
        symlink(outside.path().join("secret"), root.join("real/file-link")).expect("a link");
        assert!(open_below(&root, Path::new("real/./deeper/file")).is_ok());
        for (relative, message) in [
            ("linked/secret", "symbolic link or not a directory"),
            ("real/file-link", "symbolic link"),
            ("real/deeper", "not a regular file"),
            ("../secret", "leaves the directory"),
            ("/etc/passwd", "leaves the directory"),
            ("", "names no file"),
            ("real/deeper/file/below", "symbolic link or not a directory"),
        ] {
            let refused = open_below(&root, Path::new(relative)).expect_err(relative);
            assert!(
                refused.to_string().contains(message),
                "{relative}: {refused}"
            );
        }
        assert_eq!(
            open_below(&root, Path::new("real/absent"))
                .expect_err("absent")
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
