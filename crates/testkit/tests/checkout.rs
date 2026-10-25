use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bosun_testkit::forge::FakeForge;
use serde_json::Value;
use tempfile::TempDir;
use url::Url;

const REPOSITORY: &str = "acme/app";

const GRANTED: &[(&str, &str)] = &[("contents", "write"), ("metadata", "read")];

struct Scene {
    forge: FakeForge,
    installation: u64,
    scratch: TempDir,
}

impl Scene {
    async fn new() -> Scene {
        let scratch = tempfile::tempdir().expect("a directory");
        let forge = FakeForge::serving_git(scratch.path().join("forge")).await;
        let installation = forge.install("acme", GRANTED);
        forge.create_repository(installation, REPOSITORY);
        Scene {
            forge,
            installation,
            scratch,
        }
    }

    fn origin(&self, full_name: &str) -> String {
        let api = self.forge.api_url();
        format!(
            "http://{}:{}/{full_name}",
            api.host_str().expect("a host"),
            api.port().expect("a port")
        )
    }

    fn token(&self, permissions: &[(&str, &str)]) -> String {
        self.forge.mint(self.installation, permissions)
    }

    fn local(&self, name: &str) -> PathBuf {
        let directory = self.scratch.path().join(name);
        fs::create_dir_all(&directory).expect("a directory");
        git(
            &directory,
            None,
            &["init", "--quiet", "--initial-branch=main"],
        );
        directory
    }

    fn api(&self, path: &str) -> Url {
        Url::parse(&format!("{}{path}", self.forge.api_url())).expect("an address")
    }
}

fn header(token: &str) -> String {
    format!(
        "AUTHORIZATION: basic {}",
        STANDARD.encode(format!("x-access-token:{token}"))
    )
}

fn command(directory: &Path, token: Option<&str>, arguments: &[&str]) -> Output {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "user.name=Mona", "-c", "user.email=mona@example.org"])
        .args(["-c", "protocol.version=2"]);
    if let Some(token) = token {
        command
            .arg("-c")
            .arg(format!("http.extraheader={}", header(token)));
    }
    command
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("git cannot run: {error}"))
}

fn git(directory: &Path, token: Option<&str>, arguments: &[&str]) -> String {
    let output = command(directory, token, arguments);
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn refused(directory: &Path, token: Option<&str>, arguments: &[&str]) -> String {
    let output = command(directory, token, arguments);
    assert!(
        !output.status.success(),
        "git {arguments:?} succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn commit(directory: &Path, files: &[(&str, &str)]) -> String {
    for (path, content) in files {
        let path = directory.join(path);
        fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        fs::write(path, content).expect("a file");
    }
    git(directory, None, &["add", "--all"]);
    git(
        directory,
        None,
        &["commit", "--quiet", "--message", "change"],
    );
    git(directory, None, &["rev-parse", "HEAD"])
}

async fn get(url: Url, token: &str, accept: &str) -> (u16, String) {
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a client")
        .get(url)
        .bearer_auth(token)
        .header("accept", accept)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the fake forge does not answer: {error}"));
    let status = response.status().as_u16();
    (status, response.text().await.expect("a body"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_reaches_the_api_and_a_reading_token_fetches_the_commit_as_checkout_does() {
    let scene = Scene::new().await;
    let writer = scene.token(&[("contents", "write")]);
    let reader = scene.token(&[("contents", "read"), ("metadata", "read")]);
    let local = scene.local("local");
    let first = commit(
        &local,
        &[
            (".bosun/workflows/ci.yml", "on: push\n"),
            ("greeting.txt", "hello\n"),
        ],
    );
    let origin = format!("{}.git", scene.origin(REPOSITORY));
    git(
        &local,
        Some(&writer),
        &["push", "--quiet", &origin, "main", "main:feature"],
    );
    let (status, resolved) = get(
        scene.api("/repos/acme/app/commits/main"),
        &reader,
        "application/vnd.github.sha",
    )
    .await;
    assert_eq!((status, resolved.as_str()), (200, first.as_str()));
    let (status, file) = get(
        scene.api(&format!(
            "/repos/acme/app/contents/.bosun/workflows/ci.yml?ref={first}"
        )),
        &reader,
        "application/vnd.github+json",
    )
    .await;
    assert_eq!(status, 200, "{file}");
    let file: Value = serde_json::from_str(&file).expect("JSON");
    assert_eq!(
        STANDARD
            .decode(file["content"].as_str().expect("content").replace('\n', ""))
            .expect("base64"),
        b"on: push\n"
    );

    let number = scene.forge.open_pull_request(REPOSITORY, "main", "feature");
    let merged = scene.forge.merge_commit(REPOSITORY, number);
    git(&local, None, &["checkout", "--quiet", "-b", "feature"]);
    let second = commit(&local, &[("greeting.txt", "hello again\n")]);
    git(
        &local,
        Some(&writer),
        &["push", "--quiet", &origin, "feature"],
    );
    assert_ne!(scene.forge.merge_commit(REPOSITORY, number), merged);
    let (_, resolved) = get(
        scene.api(&format!("/repos/acme/app/commits/refs/pull/{number}/head")),
        &reader,
        "application/vnd.github.sha",
    )
    .await;
    assert_eq!(resolved, second);

    let checkout = scene.local("checkout");
    git(
        &checkout,
        None,
        &["remote", "add", "origin", &scene.origin(REPOSITORY)],
    );
    git(
        &checkout,
        Some(&reader),
        &[
            "fetch",
            "--no-tags",
            "--prune",
            "--no-recurse-submodules",
            "--depth=1",
            "origin",
            &format!("+{first}:refs/remotes/origin/main"),
        ],
    );
    git(
        &checkout,
        None,
        &[
            "checkout",
            "--quiet",
            "--force",
            "-B",
            "main",
            "refs/remotes/origin/main",
        ],
    );
    assert_eq!(
        fs::read_to_string(checkout.join("greeting.txt")).expect("the file"),
        "hello\n"
    );
    assert_eq!(git(&checkout, None, &["rev-parse", "HEAD"]), first);
}

#[tokio::test(flavor = "multi_thread")]
async fn git_answers_as_the_forge_to_missing_dead_foreign_and_reading_credentials() {
    let scene = Scene::new().await;
    let writer = scene.token(&[("contents", "write")]);
    let local = scene.local("local");
    commit(&local, &[("README.md", "# app\n")]);
    let origin = scene.origin(REPOSITORY);
    git(&local, Some(&writer), &["push", "--quiet", &origin, "main"]);

    let without = refused(&local, None, &["ls-remote", &origin]);
    assert!(without.contains("could not read Username"), "{without}");

    let reader = scene.token(&[("contents", "read")]);
    let pushing = refused(&local, Some(&reader), &["push", &origin, "main:other"]);
    assert!(
        pushing.contains("Permission to acme/app.git denied to installation."),
        "{pushing}"
    );
    assert!(pushing.contains("403"), "{pushing}");

    let other = scene.forge.install("globex", GRANTED);
    scene.forge.create_repository(other, "globex/site");
    let foreign = scene.forge.mint(other, &[("contents", "read")]);
    let missing = refused(&local, Some(&foreign), &["ls-remote", &origin]);
    assert!(missing.contains("Repository not found."), "{missing}");

    let unknown = refused(&local, Some("ghs_unknown"), &["ls-remote", &origin]);
    assert!(unknown.contains("could not read Username"), "{unknown}");

    scene.forge.publish(REPOSITORY);
    let listed = git(&local, None, &["ls-remote", &origin]);
    assert!(listed.ends_with("refs/heads/main"), "{listed}");
    let anonymous = refused(&local, None, &["push", &origin, "main:other"]);
    assert!(anonymous.contains("could not read Username"), "{anonymous}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repository_is_described_with_its_default_branch_and_visibility() {
    let scene = Scene::new().await;
    let reader = scene.token(&[("metadata", "read")]);
    let (status, described) = get(
        scene.api("/repos/acme/app"),
        &reader,
        "application/vnd.github+json",
    )
    .await;
    assert_eq!(status, 200, "{described}");
    let described: Value = serde_json::from_str(&described).expect("JSON");
    assert_eq!(described["full_name"], "acme/app");
    assert_eq!(described["default_branch"], "main");
    assert_eq!(described["private"], true);
    assert_eq!(described["owner"]["login"], "acme");

    let blind = scene.token(&[("contents", "read")]);
    let (status, _) = get(
        scene.api("/repos/acme/app"),
        &blind,
        "application/vnd.github+json",
    )
    .await;
    assert_eq!(status, 403);

    scene.forge.publish(REPOSITORY);
    let (_, described) = get(
        scene.api("/repos/acme/app"),
        &reader,
        "application/vnd.github+json",
    )
    .await;
    let described: Value = serde_json::from_str(&described).expect("JSON");
    assert_eq!(described["private"], false);
}
