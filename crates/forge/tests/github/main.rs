mod app;
mod checks;
mod contents;
mod history;
mod installations;
mod rate;
mod tarball;
mod tokens;

use bosun_forge::{GitHubApp, Repository};
use bosun_testkit::forge::{APP_ID, APP_KEY, FakeForge};

pub const OWNER: &str = "acme";

pub const NAME: &str = "app";

pub const FULL_NAME: &str = "acme/app";

pub const EVERYTHING: &[(&str, &str)] = &[
    ("checks", "write"),
    ("contents", "write"),
    ("issues", "write"),
    ("metadata", "read"),
    ("pull_requests", "write"),
    ("statuses", "write"),
];

pub struct Setup {
    pub forge: FakeForge,
    pub app: GitHubApp,
    pub installation: u64,
    pub repository_id: u64,
    pub repository: Repository,
}

pub async fn setup() -> Setup {
    let forge = FakeForge::start().await;
    let installation = forge.install(OWNER, EVERYTHING);
    let repository_id = forge.create_repository(installation, FULL_NAME);
    let app = GitHubApp::new(APP_ID, APP_KEY.as_bytes(), &forge.api_url())
        .unwrap_or_else(|error| panic!("the App does not start: {error}"));
    Setup {
        forge,
        app,
        installation,
        repository_id,
        repository: Repository {
            installation,
            owner: OWNER.to_owned(),
            name: NAME.to_owned(),
        },
    }
}
