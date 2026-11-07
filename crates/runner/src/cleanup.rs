use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustix::fs::{Access, AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use crate::container::engine::within;
use crate::container::{Call, Engine};

mod tree;

const REMOVE_WITHIN: Duration = Duration::from_secs(600);

const RESTORE_WITHIN: Duration = Duration::from_secs(60);

const DEPTH: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cleanup {
    pub roots: Vec<PathBuf>,
    pub before_download: Vec<PathBuf>,
    pub before: Vec<PathBuf>,
    pub after: Vec<PathBuf>,
}

impl Cleanup {
    pub async fn empty_before_download(
        &self,
        engine: Option<&Arc<dyn Engine>>,
    ) -> Result<(), String> {
        empty_all(&self.roots, &self.before_download, engine).await
    }
}

#[derive(Debug, Clone, Copy)]
struct Settings {
    uid: u32,
    depth: usize,
}

impl Settings {
    fn agent() -> Settings {
        Settings {
            uid: rustix::process::geteuid().as_raw(),
            depth: DEPTH,
        }
    }
}

pub(crate) async fn empty_all(
    roots: &[PathBuf],
    directories: &[PathBuf],
    engine: Option<&Arc<dyn Engine>>,
) -> Result<(), String> {
    Cleaner::new(engine, Settings::agent())
        .empty_all(roots, directories)
        .await
}

pub(crate) async fn empty(
    roots: &[PathBuf],
    directory: &Path,
    engine: Option<&Arc<dyn Engine>>,
) -> Result<(), String> {
    Cleaner::new(engine, Settings::agent())
        .empty(roots, directory)
        .await
}

enum Reach {
    Directory(OwnedFd),
    Gone,
}

struct Cleaner<'a> {
    engine: Option<&'a Arc<dyn Engine>>,
    settings: Settings,
}

impl<'a> Cleaner<'a> {
    fn new(engine: Option<&'a Arc<dyn Engine>>, settings: Settings) -> Cleaner<'a> {
        Cleaner { engine, settings }
    }

    async fn empty_all(&self, roots: &[PathBuf], directories: &[PathBuf]) -> Result<(), String> {
        let mut faults = Vec::new();
        for directory in directories {
            if let Err(fault) = self.empty(roots, directory).await {
                faults.push(fault);
            }
        }
        if faults.is_empty() {
            Ok(())
        } else {
            Err(faults.join("; "))
        }
    }

    async fn empty(&self, roots: &[PathBuf], directory: &Path) -> Result<(), String> {
        let refused = || {
            format!(
                "{} is not a directory that the agent may empty",
                directory.display()
            )
        };
        if !directory.is_absolute() || directory.parent().is_none() {
            return Err(refused());
        }
        let (root, below) = roots
            .iter()
            .filter_map(|root| {
                let below = directory.strip_prefix(root).ok()?;
                below
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                    .then_some((root, below))
            })
            .max_by_key(|(root, _)| root.components().count())
            .ok_or_else(refused)?;
        let opened = match self.reach(root, below).await {
            Ok(Reach::Directory(opened)) => opened,
            Ok(Reach::Gone) => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "{} cannot be reached: {error}",
                    directory.display()
                ));
            }
        };
        let depth = self.settings.depth;
        let failed = tokio::task::spawn_blocking(move || tree::remove_entries(opened, depth))
            .await
            .map_err(|error| format!("{} cannot be emptied: {error}", directory.display()))?
            .map_err(|error| format!("{} cannot be listed: {error}", directory.display()))?;
        let mut faults = Vec::new();
        for (name, error) in failed {
            if let Err(fault) = self.remove(&directory.join(name), error).await {
                faults.push(fault);
            }
        }
        if faults.is_empty() {
            Ok(())
        } else {
            Err(faults.join("; "))
        }
    }

    async fn reach(&self, root: &Path, below: &Path) -> Result<Reach, String> {
        let Some(mut directory) = self.open_root(root).await? else {
            return Ok(Reach::Gone);
        };
        let mut path = root.to_path_buf();
        for component in below.components() {
            let Component::Normal(name) = component else {
                return Err("it leaves the directory it lies in".to_owned());
            };
            path.push(name);
            let opened = match tree::open_directory(directory.as_fd(), name) {
                Err(Errno::ACCESS | Errno::PERM) => {
                    self.restore(&path).await?;
                    tree::open_directory(directory.as_fd(), name)
                }
                other => other,
            };
            match opened {
                Ok(next) => {
                    self.usable(&next, &path).await?;
                    directory = next;
                }
                Err(Errno::NOENT) => return Ok(Reach::Gone),
                Err(errno) => {
                    match rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(stat)
                            if FileType::from_raw_mode(stat.st_mode) == FileType::Directory =>
                        {
                            return Err(io::Error::from(errno).to_string());
                        }
                        Ok(_) => {
                            rustix::fs::unlinkat(&directory, name, AtFlags::empty())
                                .map_err(|errno| io::Error::from(errno).to_string())?;
                            return Ok(Reach::Gone);
                        }
                        Err(Errno::NOENT) => return Ok(Reach::Gone),
                        Err(other) => return Err(io::Error::from(other).to_string()),
                    }
                }
            }
        }
        Ok(Reach::Directory(directory))
    }

    async fn open_root(&self, root: &Path) -> Result<Option<OwnedFd>, String> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let opened = match rustix::fs::open(root, flags, Mode::empty()) {
            Err(Errno::ACCESS) => {
                self.restore(root).await?;
                rustix::fs::open(root, flags, Mode::empty())
            }
            other => other,
        };
        match opened {
            Ok(opened) => {
                self.usable(&opened, root).await?;
                Ok(Some(opened))
            }
            Err(Errno::NOENT) => Ok(None),
            Err(errno) => Err(io::Error::from(errno).to_string()),
        }
    }

    async fn usable(&self, directory: &OwnedFd, path: &Path) -> Result<(), String> {
        let stat = rustix::fs::fstat(directory).map_err(|errno| {
            format!(
                "{} cannot be examined: {}",
                path.display(),
                io::Error::from(errno)
            )
        })?;
        if stat.st_uid == self.settings.uid {
            if (stat.st_mode & 0o700) == 0o700 {
                return Ok(());
            }
            if rustix::fs::fchmod(directory, Mode::from_raw_mode(stat.st_mode | 0o700)).is_ok() {
                return Ok(());
            }
        } else if rustix::fs::accessat(
            directory,
            ".",
            Access::READ_OK | Access::WRITE_OK | Access::EXEC_OK,
            AtFlags::empty(),
        )
        .is_ok()
        {
            return Ok(());
        }
        self.restore(path).await
    }

    async fn restore(&self, path: &Path) -> Result<(), String> {
        let Some(engine) = self.engine else {
            return Err(format!(
                "{} is closed to the agent, and the agent has no podman to open it",
                path.display()
            ));
        };
        for (program, argument) in [("chown", "0:0"), ("chmod", "u+rwx")] {
            let call = Call::new([
                OsString::from("unshare"),
                program.into(),
                "--".into(),
                argument.into(),
                path.as_os_str().to_owned(),
            ]);
            match within(engine, call, RESTORE_WITHIN).await {
                Ok(outcome) if outcome.success() => {}
                Ok(outcome) => {
                    return Err(format!(
                        "{} could not be opened to the agent: podman unshare {program} {}",
                        path.display(),
                        outcome.failure()
                    ));
                }
                Err(unrun) => {
                    return Err(format!(
                        "{} could not be opened to the agent: podman unshare {program} could \
                         not run: {unrun}",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
    }

    async fn remove(&self, path: &Path, error: io::Error) -> Result<(), String> {
        if error.kind() == ErrorKind::NotFound {
            return Ok(());
        }
        let Some(engine) = self.engine else {
            return Err(format!("{} cannot be removed: {error}", path.display()));
        };
        let unshared = within(
            engine,
            Call::new([
                OsString::from("unshare"),
                "rm".into(),
                "-rf".into(),
                "--".into(),
                path.as_os_str().to_owned(),
            ]),
            REMOVE_WITHIN,
        )
        .await;
        match unshared {
            Ok(outcome) if outcome.success() => Ok(()),
            Ok(outcome) => Err(format!(
                "{} cannot be removed: {error}, and podman unshare {}",
                path.display(),
                outcome.failure()
            )),
            Err(unrun) => Err(format!(
                "{} cannot be removed: {error}, and podman unshare could not run: {unrun}",
                path.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::slice::from_ref;
    use std::sync::{Arc, Mutex, PoisonError};

    use super::{Cleaner, DEPTH, Settings, empty, empty_all};
    use crate::container::{Call, Engine, Outcome, Pending};

    fn base() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("a directory");
        let base = fs::canonicalize(directory.path()).expect("a path");
        (directory, base)
    }

    fn victim(base: &Path) -> PathBuf {
        let victim = base.join("victim");
        fs::create_dir_all(victim.join("repo/nested")).expect("directories");
        fs::write(victim.join("kept"), "x").expect("a file");
        fs::write(victim.join("repo/nested/kept"), "x").expect("a file");
        victim
    }

    fn untouched(victim: &Path) -> bool {
        victim.join("kept").exists() && victim.join("repo/nested/kept").exists()
    }

    #[tokio::test]
    async fn a_directory_is_emptied_but_stays_and_a_link_is_removed_without_being_followed() {
        let (_guard, base) = base();
        let outside = victim(&base);
        let target = base.join("work");
        fs::create_dir_all(target.join("nested/deeper")).expect("directories");
        fs::write(target.join("nested/deeper/file"), "x").expect("a file");
        fs::write(target.join("top"), "x").expect("a file");
        symlink(&outside, target.join("link")).expect("a link");
        empty(from_ref(&target), &target, None)
            .await
            .expect("emptied");
        assert!(target.is_dir());
        assert_eq!(fs::read_dir(&target).expect("listed").count(), 0);
        assert!(untouched(&outside));
    }

    #[tokio::test]
    async fn a_link_in_the_place_of_the_directory_is_removed_and_its_target_stays_untouched() {
        let (_guard, base) = base();
        let outside = victim(&base);
        let work = base.join("work");
        fs::create_dir_all(&work).expect("a directory");
        let target = work.join("globalart.keel");
        symlink(&outside, &target).expect("a link");
        empty(from_ref(&work), &target, None)
            .await
            .expect("a link is no fault");
        assert!(fs::symlink_metadata(&target).is_err());
        assert!(untouched(&outside));
        assert!(work.is_dir());
    }

    #[tokio::test]
    async fn a_link_in_the_middle_of_the_path_is_removed_and_nothing_behind_it_is_emptied() {
        let (_guard, base) = base();
        let outside = victim(&base);
        let work = base.join("work");
        fs::create_dir_all(&work).expect("a directory");
        symlink(&outside, work.join("globalart.keel")).expect("a link");
        empty(from_ref(&work), &work.join("globalart.keel/repo"), None)
            .await
            .expect("a link is no fault");
        assert!(fs::symlink_metadata(work.join("globalart.keel")).is_err());
        assert!(untouched(&outside));
    }

    #[tokio::test]
    async fn a_file_in_the_place_of_a_directory_is_removed_and_a_nested_target_is_emptied() {
        let (_guard, base) = base();
        let work = base.join("work");
        fs::create_dir_all(&work).expect("a directory");
        fs::write(work.join("_actions"), "x").expect("a file");
        empty(from_ref(&work), &work.join("_actions"), None)
            .await
            .expect("a file is no fault");
        assert!(fs::symlink_metadata(work.join("_actions")).is_err());
        let nested = work.join("globalart.keel/keel");
        fs::create_dir_all(nested.join("src")).expect("directories");
        fs::write(nested.join("src/file"), "x").expect("a file");
        fs::write(work.join("globalart.keel/other"), "x").expect("a file");
        empty(from_ref(&work), &nested, None)
            .await
            .expect("emptied");
        assert!(nested.is_dir());
        assert_eq!(fs::read_dir(&nested).expect("listed").count(), 0);
        assert!(work.join("globalart.keel/other").exists());
    }

    #[tokio::test]
    async fn a_root_behind_a_link_of_the_operator_is_followed_and_what_is_below_it_is_not() {
        let (_guard, base) = base();
        let disk = base.join("disk");
        fs::create_dir_all(disk.join("sub")).expect("directories");
        fs::write(disk.join("sub/file"), "x").expect("a file");
        let work = base.join("work");
        symlink(&disk, &work).expect("a link");
        empty(from_ref(&work), &work, None).await.expect("emptied");
        assert!(disk.is_dir());
        assert_eq!(fs::read_dir(&disk).expect("listed").count(), 0);
    }

    #[tokio::test]
    async fn a_directory_the_job_made_unwritable_is_still_removed() {
        let (_guard, base) = base();
        let work = base.join("work");
        let locked = work.join("locked");
        fs::create_dir_all(&locked).expect("directories");
        fs::write(locked.join("file"), "x").expect("a file");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).expect("permissions");
        empty(from_ref(&work), &work, None).await.expect("emptied");
        assert_eq!(fs::read_dir(&work).expect("listed").count(), 0);
        let closed = work.join("closed");
        fs::create_dir_all(closed.join("inside")).expect("directories");
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).expect("permissions");
        empty(from_ref(&work), &closed, None)
            .await
            .expect("emptied");
        assert_eq!(fs::read_dir(&closed).expect("listed").count(), 0);
    }

    #[tokio::test]
    async fn a_missing_directory_is_empty_and_what_lies_outside_every_root_is_refused() {
        let (_guard, base) = base();
        let work = base.join("work");
        empty(from_ref(&work), &work.join("absent"), None)
            .await
            .expect("nothing to empty");
        empty(from_ref(&work), &work, None)
            .await
            .expect("nothing to empty");
        for refused in [
            "/".to_owned(),
            "relative/dir".to_owned(),
            base.display().to_string(),
            work.join("../escape").display().to_string(),
        ] {
            let error = empty_all(from_ref(&work), &[refused.clone().into()], None)
                .await
                .expect_err("a refusal");
            assert!(
                error.contains("not a directory that the agent may empty"),
                "{refused}: {error}"
            );
        }
    }

    #[derive(Debug, Default)]
    struct Unshared {
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Unshared {
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl Engine for Unshared {
        fn run(&self, call: Call) -> Pending<'_, io::Result<Outcome>> {
            Box::pin(async move {
                let words: Vec<String> = call
                    .arguments
                    .iter()
                    .map(|word| word.to_string_lossy().into_owned())
                    .collect();
                self.calls
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(words.clone());
                let [unshare, program, rest @ ..] = words.as_slice() else {
                    return Err(io::Error::other("not a call to unshare"));
                };
                assert_eq!(unshare, "unshare");
                let code = if program == "chown" {
                    0
                } else {
                    Command::new(program)
                        .args(rest)
                        .status()?
                        .code()
                        .unwrap_or(-1)
                };
                Ok(Outcome {
                    code: Some(code),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            })
        }

        fn exec(
            &self,
            _container: &str,
            _launcher: &str,
        ) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
            unreachable!("a cleaning never executes a step")
        }
    }

    fn unshared() -> (Arc<Unshared>, Arc<dyn Engine>) {
        let fake = Arc::new(Unshared::default());
        let engine: Arc<dyn Engine> = Arc::clone(&fake) as Arc<dyn Engine>;
        (fake, engine)
    }

    fn words(call: &[&str], path: &Path) -> Vec<String> {
        call.iter()
            .map(|word| (*word).to_owned())
            .chain([path.display().to_string()])
            .collect()
    }

    fn opened_up(path: &Path) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("permissions");
    }

    #[tokio::test]
    async fn a_root_the_job_closed_is_opened_again_through_podman_unshare_before_it_is_used() {
        let (_guard, base) = base();
        let work = base.join("work");
        fs::create_dir_all(work.join("nested")).expect("directories");
        fs::write(work.join("nested/file"), "x").expect("a file");
        fs::set_permissions(&work, fs::Permissions::from_mode(0o000)).expect("permissions");
        let refused = empty(from_ref(&work), &work, None)
            .await
            .expect_err("a closed root without podman");
        assert!(
            refused.contains("cannot be reached") && refused.contains("no podman"),
            "{refused}"
        );
        let (fake, engine) = unshared();
        empty(from_ref(&work), &work, Some(&engine))
            .await
            .expect("emptied");
        assert_eq!(fs::read_dir(&work).expect("listed").count(), 0);
        assert_eq!(
            fake.calls(),
            [
                words(&["unshare", "chown", "--", "0:0"], &work),
                words(&["unshare", "chmod", "--", "u+rwx"], &work)
            ]
        );
        opened_up(&work);
    }

    #[tokio::test]
    async fn a_root_given_to_another_user_is_taken_back_through_podman_unshare_and_one_of_the_agent_is_not()
     {
        let (_guard, base) = base();
        let work = base.join("work");
        fs::create_dir_all(work.join("nested")).expect("directories");
        fs::write(work.join("nested/file"), "x").expect("a file");
        fs::set_permissions(&work, fs::Permissions::from_mode(0o555)).expect("permissions");
        let me = fs::metadata(&work).expect("metadata").uid();
        let (fake, engine) = unshared();
        let stranger = Cleaner::new(
            Some(&engine),
            Settings {
                uid: me + 1,
                depth: DEPTH,
            },
        );
        stranger
            .empty(from_ref(&work), &work)
            .await
            .expect("emptied");
        assert_eq!(fs::read_dir(&work).expect("listed").count(), 0);
        assert_eq!(
            fake.calls(),
            [
                words(&["unshare", "chown", "--", "0:0"], &work),
                words(&["unshare", "chmod", "--", "u+rwx"], &work)
            ]
        );
        fs::create_dir_all(work.join("again")).expect("a directory");
        fs::set_permissions(&work, fs::Permissions::from_mode(0o555)).expect("permissions");
        let (quiet, engine) = unshared();
        Cleaner::new(
            Some(&engine),
            Settings {
                uid: me,
                depth: DEPTH,
            },
        )
        .empty(from_ref(&work), &work)
        .await
        .expect("emptied by the agent itself");
        assert!(quiet.calls().is_empty());
        assert_eq!(fs::read_dir(&work).expect("listed").count(), 0);
    }

    #[tokio::test]
    async fn a_tree_deeper_than_the_walk_allows_is_removed_by_podman_unshare_and_the_rest_is_not() {
        let (_guard, base) = base();
        let work = base.join("work");
        let mut deep = work.join("deep");
        for level in 0..12 {
            deep.push(format!("level-{level}"));
        }
        fs::create_dir_all(&deep).expect("directories");
        fs::write(deep.join("file"), "x").expect("a file");
        fs::create_dir_all(work.join("shallow/a")).expect("directories");
        fs::write(work.join("shallow/a/file"), "x").expect("a file");
        let me = fs::metadata(&work).expect("metadata").uid();
        let settings = Settings { uid: me, depth: 4 };
        let refused = Cleaner::new(None, settings)
            .empty(from_ref(&work), &work)
            .await
            .expect_err("a tree that is too deep without podman");
        assert!(refused.contains("nested deeper than 4 levels"), "{refused}");
        assert!(!work.join("shallow").exists());
        let (fake, engine) = unshared();
        Cleaner::new(Some(&engine), settings)
            .empty(from_ref(&work), &work)
            .await
            .expect("emptied");
        assert_eq!(fs::read_dir(&work).expect("listed").count(), 0);
        assert_eq!(
            fake.calls(),
            [words(&["unshare", "rm", "-rf", "--"], &work.join("deep"))]
        );
    }
}
