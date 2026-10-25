use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use bosun_types::error::ErrorEnvelope;
use flate2::read::GzDecoder;
use lzma_rust2::XzReader;
use reqwest::StatusCode;
use tokio::io::AsyncWriteExt;
use tracing::info;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compression {
    Gzip,
    Xz,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Archive {
    pub(crate) path: String,
    pub(crate) compression: Compression,
    pub(crate) into: PathBuf,
    pub(crate) marker: Option<(PathBuf, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fetches {
    pub(crate) url: String,
    pub(crate) credential: String,
    pub(crate) archives: Vec<Archive>,
}

impl Fetches {
    pub(crate) async fn run(self) -> Result<(), String> {
        let pending: Vec<Archive> = self
            .archives
            .into_iter()
            .filter(|archive| !archive.present())
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        let client = reqwest::Client::builder()
            .user_agent(concat!("bosun-agent/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .no_proxy()
            .build()
            .map_err(|error| format!("the agent has no HTTP client: {error}"))?;
        let base = self.url.trim_end_matches('/');
        for archive in pending {
            let address = format!("{base}{}", archive.path);
            let download = sibling(&archive.into, "download");
            download_to(&client, &address, &self.credential, &download).await?;
            let unpacked = tokio::task::spawn_blocking({
                let archive = archive.clone();
                let download = download.clone();
                move || archive.install(&download)
            })
            .await
            .map_err(|error| format!("the archive {address} could not be unpacked: {error}"))?;
            let _ = fs::remove_file(&download);
            unpacked.map_err(|problem| format!("the archive {address} {problem}"))?;
            info!(address, directory = %archive.into.display(), "fetched an archive");
        }
        Ok(())
    }
}

impl Archive {
    fn present(&self) -> bool {
        self.into.is_dir()
            && self.marker.as_ref().is_none_or(|(file, content)| {
                fs::read_to_string(file).is_ok_and(|found| found == *content)
            })
    }

    fn install(&self, download: &Path) -> Result<(), String> {
        let partial = sibling(&self.into, "partial");
        remove(&partial).map_err(|error| format!("left {}: {error}", partial.display()))?;
        let unpacked = self.unpack(download, &partial).and_then(|top| {
            if let Some((file, _)) = &self.marker {
                remove(file).map_err(|error| format!("left {}: {error}", file.display()))?;
            }
            remove(&self.into).map_err(|error| format!("left {}: {error}", self.into.display()))?;
            fs::rename(&top, &self.into).map_err(|error| {
                format!("could not be moved to {}: {error}", self.into.display())
            })?;
            match &self.marker {
                Some((file, content)) => fs::write(file, content)
                    .map_err(|error| format!("left {} unwritten: {error}", file.display())),
                None => Ok(()),
            }
        });
        let _ = remove(&partial);
        unpacked
    }

    fn unpack(&self, download: &Path, partial: &Path) -> Result<PathBuf, String> {
        let file = File::open(download).map_err(|error| format!("cannot be read: {error}"))?;
        let reader: Box<dyn Read> = match self.compression {
            Compression::Gzip => Box::new(GzDecoder::new(BufReader::new(file))),
            Compression::Xz => Box::new(XzReader::new(BufReader::new(file), true)),
        };
        fs::create_dir_all(partial)
            .map_err(|error| format!("has no directory {}: {error}", partial.display()))?;
        tar::Archive::new(reader)
            .unpack(partial)
            .map_err(|error| format!("is not a readable tar archive: {error}"))?;
        let mut tops = fs::read_dir(partial)
            .map_err(|error| format!("cannot be listed: {error}"))?
            .flatten()
            .map(|entry| entry.path());
        match (tops.next(), tops.next()) {
            (Some(top), None) if top.is_dir() => Ok(top),
            _ => Err("does not hold exactly one directory at its top".to_owned()),
        }
    }
}

fn sibling(directory: &Path, suffix: &str) -> PathBuf {
    let name = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    directory.with_file_name(format!(".{name}.{suffix}"))
}

fn remove(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn download_to(
    client: &reqwest::Client,
    address: &str,
    credential: &str,
    file: &Path,
) -> Result<(), String> {
    let unreachable = |error: reqwest::Error| format!("{address} could not be fetched: {error}");
    let mut response = client
        .get(address)
        .bearer_auth(credential)
        .send()
        .await
        .map_err(unreachable)?;
    let status = response.status();
    if status != StatusCode::OK {
        let text = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<ErrorEnvelope>(&text)
            .map(|envelope| envelope.error.message)
            .unwrap_or(text);
        return Err(format!("{address} answered {status}: {message}"));
    }
    if let Some(parent) = file.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("{} cannot be created: {error}", parent.display()))?;
    }
    let written = |error: std::io::Error| {
        format!(
            "{address} could not be written to {}: {error}",
            file.display()
        )
    };
    let mut output = tokio::fs::File::create(file).await.map_err(written)?;
    while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
        output.write_all(&chunk).await.map_err(written)?;
    }
    output.flush().await.map_err(written)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use flate2::Compression as Level;
    use flate2::write::GzEncoder;

    use super::{Archive, Compression};

    fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Level::fast()));
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, path, *content)
                .expect("an entry");
        }
        builder
            .into_inner()
            .expect("an archive")
            .finish()
            .expect("a tarball")
    }

    fn archive(into: &Path, marker: Option<(&Path, &str)>) -> Archive {
        Archive {
            path: "/_runtime/actions/a/b/c.tar.gz".to_owned(),
            compression: Compression::Gzip,
            into: into.to_path_buf(),
            marker: marker.map(|(file, content)| (file.to_path_buf(), content.to_owned())),
        }
    }

    #[test]
    fn an_archive_replaces_its_directory_with_its_top_directory_and_writes_its_marker() {
        let root = tempfile::tempdir().expect("a directory");
        let download = root.path().join("download");
        let into = root.path().join("externals/node24");
        let marker = root.path().join("externals/node24.version");
        fs::create_dir_all(into.join("old")).expect("an older runtime");
        fs::write(
            &download,
            tarball(&[("node-v24.21.0-linux-x64/bin/node", b"node")]),
        )
        .expect("an archive");
        let node = archive(&into, Some((&marker, "24.21.0 linux-x64")));
        assert!(!node.present());
        node.install(&download).expect("an installed archive");
        assert_eq!(fs::read(into.join("bin/node")).expect("node"), b"node");
        assert!(!into.join("old").exists());
        assert!(node.present());
        assert!(!archive(&into, Some((&marker, "24.22.1 linux-x64"))).present());
        let left: Vec<String> = fs::read_dir(root.path().join("externals"))
            .expect("the externals")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
    }

    #[test]
    fn an_archive_without_a_single_top_directory_is_refused_and_leaves_nothing() {
        let root = tempfile::tempdir().expect("a directory");
        let download = root.path().join("download");
        let into = root.path().join("_actions/a/b/c");
        fs::write(&download, tarball(&[("one/a", b"a"), ("two/b", b"b")])).expect("an archive");
        let refused = archive(&into, None)
            .install(&download)
            .expect_err("a refusal");
        assert_eq!(refused, "does not hold exactly one directory at its top");
        assert!(!into.exists());
        assert_eq!(
            fs::read_dir(root.path().join("_actions/a/b"))
                .expect("the parent")
                .count(),
            0
        );
        fs::write(&download, b"not a tarball").expect("a file");
        let refused = archive(&into, None)
            .install(&download)
            .expect_err("a refusal");
        assert!(
            refused.starts_with("is not a readable tar archive"),
            "{refused}"
        );
    }
}
