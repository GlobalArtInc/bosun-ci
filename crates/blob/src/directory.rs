use std::fmt;
use std::io::{self, SeekFrom};
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};

use crate::key::starts_some_key;
use crate::{Error, Key, Object, Store, clip};

const OBJECTS: &str = "objects";
const STAGING: &str = "staging";
const WRITE_BUFFER: usize = 256 * 1024;
const PUBLISH_ATTEMPTS: usize = 8;

static STAGED: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct Directory {
    objects: PathBuf,
    staging: PathBuf,
}

impl Directory {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, Error> {
        let root = root.as_ref();
        let directory = Self {
            objects: root.join(OBJECTS),
            staging: root.join(STAGING),
        };
        for path in [&directory.objects, &directory.staging] {
            fs::create_dir_all(path)
                .await
                .map_err(|source| io_error(path.display(), source))?;
        }
        Ok(directory)
    }

    fn path(&self, key: &Key) -> PathBuf {
        let mut path = self.objects.clone();
        path.extend(key.segments());
        path
    }

    async fn prune(&self, path: &Path) {
        let mut directory = path.parent();
        while let Some(current) = directory
            && current != self.objects
        {
            if fs::remove_dir(current).await.is_err() {
                break;
            }
            directory = current.parent();
        }
    }
}

impl Store for Directory {
    async fn put<R>(&self, key: &Key, mut body: R) -> Result<(), Error>
    where
        R: AsyncRead + Send + Unpin,
    {
        let failed = |source: io::Error| io_error(key, source);
        let (mut staged, file) = Staged::create(&self.staging).await.map_err(failed)?;
        let mut writer = BufWriter::with_capacity(WRITE_BUFFER, file);
        tokio::io::copy(&mut body, &mut writer)
            .await
            .map_err(failed)?;
        writer.flush().await.map_err(failed)?;
        let file = writer.into_inner();
        file.sync_all().await.map_err(failed)?;
        drop(file);
        let target = self.path(key);
        publish(&staged.path, &target).await.map_err(failed)?;
        staged.published = true;
        match target.parent() {
            Some(parent) => sync_directory(parent).await.map_err(failed),
            None => Ok(()),
        }
    }

    async fn get<B>(&self, key: &Key, range: B) -> Result<Object, Error>
    where
        B: RangeBounds<u64> + Send,
    {
        let failed = |source: io::Error| io_error(key, source);
        let mut file = match File::open(self.path(key)).await {
            Ok(file) => file,
            Err(error) if absent(&error) => return Err(Error::NotFound(key.clone())),
            Err(error) => return Err(failed(error)),
        };
        let metadata = file.metadata().await.map_err(failed)?;
        if !metadata.is_file() {
            return Err(Error::NotFound(key.clone()));
        }
        let range = clip(key, range, metadata.len())?;
        if range.start > 0 {
            file.seek(SeekFrom::Start(range.start))
                .await
                .map_err(failed)?;
        }
        Ok(Object::new(
            metadata.len(),
            file.take(range.end - range.start),
        ))
    }

    async fn delete(&self, key: &Key) -> Result<(), Error> {
        let path = self.path(key);
        match fs::remove_file(&path).await {
            Ok(()) => {
                self.prune(&path).await;
                Ok(())
            }
            Err(error) if absent(&error) => Ok(()),
            Err(error) => match fs::symlink_metadata(&path).await {
                Ok(metadata) if metadata.is_dir() => Ok(()),
                _ => Err(io_error(key, error)),
            },
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<Key>, Error> {
        if !starts_some_key(prefix) {
            return Ok(Vec::new());
        }
        let failed = |source: io::Error| io_error(prefix, source);
        let mut start = self.objects.clone();
        let (above, partial) = match prefix.rsplit_once('/') {
            Some((directory, partial)) => {
                start.extend(directory.split('/'));
                (format!("{directory}/"), partial)
            }
            None => (String::new(), prefix),
        };
        let mut keys = Vec::new();
        let mut pending = vec![(start, above, partial)];
        while let Some((path, above, partial)) = pending.pop() {
            let mut entries = match fs::read_dir(&path).await {
                Ok(entries) => entries,
                Err(error) if absent(&error) => continue,
                Err(error) => return Err(failed(error)),
            };
            while let Some(entry) = entries.next_entry().await.map_err(failed)? {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if !name.starts_with(partial) {
                    continue;
                }
                let kind = match entry.file_type().await {
                    Ok(kind) => kind,
                    Err(error) if absent(&error) => continue,
                    Err(error) => return Err(failed(error)),
                };
                let key = format!("{above}{name}");
                if kind.is_dir() {
                    pending.push((entry.path(), format!("{key}/"), ""));
                } else if kind.is_file()
                    && let Ok(key) = Key::new(key)
                {
                    keys.push(key);
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
}

struct Staged {
    path: PathBuf,
    published: bool,
}

impl Staged {
    async fn create(staging: &Path) -> io::Result<(Self, File)> {
        loop {
            let name = format!(
                "{}-{}",
                process::id(),
                STAGED.fetch_add(1, Ordering::Relaxed)
            );
            let path = staging.join(name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                Ok(file) => {
                    return Ok((
                        Self {
                            path,
                            published: false,
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn publish(staged: &Path, target: &Path) -> io::Result<()> {
    let mut attempts = 1;
    loop {
        let result = match target.parent() {
            Some(parent) => fs::create_dir_all(parent).await,
            None => Ok(()),
        };
        let result = match result {
            Ok(()) => fs::rename(staged, target).await,
            Err(error) => Err(error),
        };
        match result {
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && attempts < PUBLISH_ATTEMPTS =>
            {
                attempts += 1;
            }
            result => return result,
        }
    }
}

async fn sync_directory(path: &Path) -> io::Result<()> {
    match File::open(path).await {
        Ok(directory) => directory.sync_all().await,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn io_error(target: impl fmt::Display, source: io::Error) -> Error {
    Error::Io {
        target: target.to_string(),
        source,
    }
}
