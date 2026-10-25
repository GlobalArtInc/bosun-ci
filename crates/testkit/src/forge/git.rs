use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use flate2::Compression;
use flate2::write::GzEncoder;
use sha1::{Digest, Sha1};

pub(super) type Tree = BTreeMap<String, String>;

const BASE64_LINE: usize = 60;

pub(super) enum Node {
    File { sha: String },
    Directory { sha: String },
}

pub(super) enum Change {
    Added,
    Modified,
    Removed,
}

impl Change {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Modified => "modified",
            Change::Removed => "removed",
        }
    }
}

pub(super) fn blob_sha(content: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", content.len()));
    hasher.update(content);
    hex(&hasher.finalize())
}

pub(super) fn commit_sha(parent: Option<&str>, tree: &Tree, sequence: usize) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("commit {sequence}\n"));
    hasher.update(parent.unwrap_or_default());
    for (path, sha) in tree {
        hasher.update(format!("\n{path} {sha}"));
    }
    hex(&hasher.finalize())
}

pub(super) fn listing(tree: &Tree, directory: &str) -> Option<Vec<(String, Node)>> {
    let directory = directory.trim_matches('/');
    let below = below(tree, directory);
    if below.is_empty() && !directory.is_empty() {
        return None;
    }
    let mut files = Vec::new();
    let mut directories: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (rest, sha) in below {
        match rest.split_once('/') {
            Some((name, inner)) => directories.entry(name).or_default().push((inner, sha)),
            None => files.push((
                rest.to_owned(),
                Node::File {
                    sha: sha.to_owned(),
                },
            )),
        }
    }
    let mut entries: Vec<(String, Node)> = directories
        .into_iter()
        .map(|(name, inner)| {
            (
                name.to_owned(),
                Node::Directory {
                    sha: tree_sha(&inner),
                },
            )
        })
        .collect();
    entries.extend(files);
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Some(entries)
}

pub(super) fn diff(base: &Tree, head: &Tree) -> Vec<(String, Change)> {
    let mut changes: Vec<(String, Change)> = head
        .iter()
        .filter_map(|(path, sha)| match base.get(path) {
            None => Some((path.clone(), Change::Added)),
            Some(before) if before != sha => Some((path.clone(), Change::Modified)),
            Some(_) => None,
        })
        .collect();
    changes.extend(
        base.keys()
            .filter(|path| !head.contains_key(*path))
            .map(|path| (path.clone(), Change::Removed)),
    );
    changes.sort_by(|left, right| left.0.cmp(&right.0));
    changes
}

pub(super) fn tarball(prefix: &str, tree: &Tree, blobs: &HashMap<String, Vec<u8>>) -> Vec<u8> {
    let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut directory = tar::Header::new_gnu();
    directory.set_entry_type(tar::EntryType::Directory);
    directory.set_mode(0o755);
    directory.set_size(0);
    directory.set_mtime(0);
    archive
        .append_data(&mut directory, format!("{prefix}/"), std::io::empty())
        .unwrap_or_else(|error| panic!("the fake forge cannot write a tarball: {error}"));
    for (path, sha) in tree {
        let content = blobs
            .get(sha)
            .unwrap_or_else(|| panic!("the fake forge lost the blob {sha}"));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(content.len() as u64);
        header.set_mtime(0);
        archive
            .append_data(&mut header, format!("{prefix}/{path}"), content.as_slice())
            .unwrap_or_else(|error| panic!("the fake forge cannot write a tarball: {error}"));
    }
    archive
        .into_inner()
        .unwrap_or_else(|error| panic!("the fake forge cannot finish a tarball: {error}"))
        .finish()
        .unwrap_or_else(|error| panic!("the fake forge cannot compress a tarball: {error}"))
}

pub(super) fn base64_lines(content: &[u8]) -> String {
    let encoded = STANDARD.encode(content);
    let mut lines = String::with_capacity(encoded.len() + encoded.len() / BASE64_LINE + 1);
    for chunk in encoded.as_bytes().chunks(BASE64_LINE) {
        lines.push_str(&String::from_utf8_lossy(chunk));
        lines.push('\n');
    }
    lines
}

pub(super) fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn below<'t>(tree: &'t Tree, directory: &str) -> Vec<(&'t str, &'t str)> {
    tree.iter()
        .filter_map(|(path, sha)| {
            let rest = if directory.is_empty() {
                Some(path.as_str())
            } else {
                path.strip_prefix(directory)?.strip_prefix('/')
            };
            rest.map(|rest| (rest, sha.as_str()))
        })
        .collect()
}

fn tree_sha(entries: &[(&str, &str)]) -> String {
    let mut hasher = Sha1::new();
    hasher.update("tree");
    for (path, sha) in entries {
        hasher.update(format!("\n{path} {sha}"));
    }
    hex(&hasher.finalize())
}
