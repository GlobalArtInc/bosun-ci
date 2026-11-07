use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::dirty;
use super::engine::within;
use super::options::{self, Policy};
use super::{
    Call, ContainerJob, Credentials, Engine, Layout, Libc, Outcome, Provision, PullPolicy, WORK,
    volumes,
};

pub(crate) const LABEL: &str = "bosun.execution";

pub(crate) const OWNER: &str = "bosun.agent";

const ANSWER_WITHIN: Duration = Duration::from_secs(120);

const TEARDOWN_WITHIN: Duration = Duration::from_secs(60);

const SIGNAL_WITHIN: Duration = Duration::from_secs(10);

const LIBC_PROBE: &str = "m=0; g=0; for d in /lib /lib64; do \
     for f in \"$d\"/ld-musl-*; do [ -e \"$f\" ] && m=1; done; \
     for f in \"$d\"/ld-linux*; do [ -e \"$f\" ] && g=1; done; done; \
     if [ \"$m\" = 1 ] && [ \"$g\" = 0 ]; then echo musl; else echo glibc; fi";

pub(crate) struct Request {
    pub(crate) workspace: PathBuf,
    pub(crate) options: Option<String>,
    pub(crate) volumes: Vec<String>,
    pub(crate) ports: Vec<String>,
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) nodes: bool,
}

pub(crate) struct Container {
    engine: Arc<dyn Engine>,
    provision: Option<Arc<dyn Provision>>,
    execution: String,
    owner: String,
    name: String,
    network: String,
    id: Option<String>,
    layout: Layout,
    root: PathBuf,
    state: PathBuf,
    environment: BTreeMap<String, String>,
}

impl Container {
    pub(crate) fn new(job: &ContainerJob, execution: &str, layout: Layout) -> Container {
        Container {
            engine: Arc::clone(&job.engine),
            provision: job.provision.clone(),
            execution: execution.to_owned(),
            owner: owner(&job.state),
            name: format!("bosun-job-{execution}"),
            network: format!("bosun-{execution}"),
            id: None,
            layout,
            root: job.state.clone(),
            state: job.state.join(execution),
            environment: BTreeMap::new(),
        }
    }

    pub(crate) fn id(&self) -> &str {
        self.id.as_deref().unwrap_or_default()
    }

    pub(crate) fn network(&self) -> &str {
        &self.network
    }

    pub(crate) fn layout(&self) -> &Layout {
        &self.layout
    }

    pub(crate) fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub(crate) fn translate(&self, host: &Path) -> Option<String> {
        self.layout.to_container(host).or_else(|| {
            fs::canonicalize(host)
                .ok()
                .and_then(|real| self.layout.to_container(&real))
        })
    }

    pub(crate) fn exec(&self, launcher: &str) -> (OsString, Vec<OsString>) {
        self.engine.exec(&self.name, launcher)
    }

    pub(crate) async fn start(
        &mut self,
        job: &ContainerJob,
        request: Request,
    ) -> Result<Vec<PathBuf>, String> {
        let Request {
            workspace,
            options,
            volumes: mounted,
            ports,
            environment,
            nodes,
        } = request;
        let policy = Policy {
            allow_privileged: job.allow_privileged && !job.untrusted,
            untrusted: job.untrusted,
            workspace: &workspace,
        };
        let image = image(&job.image)?;
        let credentials = used(job.credentials.as_ref())?;
        let mut flags = match options.as_deref() {
            Some(text) => {
                options::arguments(text, &policy).map_err(|refusal| refusal.to_string())?
            }
            None => Vec::new(),
        };
        for volume in &mounted {
            if let Some(why) = volumes::malformed(volume, false) {
                return Err(format!(
                    "the volume `{}` of `container` {why}, which podman may read differently \
                     from the agent",
                    volume.escape_debug()
                ));
            }
            volumes::volume(volume, &policy).or_else(|reason| {
                if policy.allow_privileged {
                    Ok(())
                } else {
                    Err(format!(
                        "the volume `{volume}` of `container` {reason}, and the pool does not set \
                         `allow_privileged`"
                    ))
                }
            })?;
            flags.push(format!("--volume={volume}"));
        }
        if policy.untrusted && !ports.is_empty() {
            return Err(
                "`ports` of `container` publishes a port on the host, which a pool of untrusted \
                 code never does"
                    .to_owned(),
            );
        }
        for port in &ports {
            if port.trim().is_empty() || options::unreadable(port).is_some() {
                return Err(format!(
                    "the port `{}` of `container` is not a port",
                    port.escape_debug()
                ));
            }
            flags.push(format!("--publish={port}"));
        }
        let own_network = !flags
            .iter()
            .any(|flag| flag.starts_with("--network=") || flag.starts_with("--net="));
        self.environment = environment;
        self.pull(job, credentials, &image).await?;
        self.run(
            "the network of the job could not be created",
            Call::new([
                "network",
                "create",
                "--label",
                &format!("{LABEL}={}", self.execution),
                "--label",
                &format!("{OWNER}={}", self.owner),
                &self.network,
            ]),
        )
        .await?;
        let create = self.create_arguments(&image, flags, own_network, &workspace)?;
        let created = self
            .run("the job container could not be created", Call::new(create))
            .await?;
        let id = created.stdout.trim().to_owned();
        if id.len() < 12 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!(
                "podman answered the creation of the job container with `{id}`, which is not a \
                 container id"
            ));
        }
        self.id = Some(id);
        self.run(
            "the job container could not be started",
            Call::new(["start", &self.name]),
        )
        .await?;
        if !nodes {
            return Ok(Vec::new());
        }
        let libc = self.libc().await?;
        match &self.provision {
            Some(provision) => provision.nodes(libc).await,
            None => Ok(Vec::new()),
        }
    }

    fn create_arguments(
        &self,
        image: &str,
        flags: Vec<String>,
        own_network: bool,
        workspace: &Path,
    ) -> Result<Vec<String>, String> {
        let mut arguments: Vec<String> = [
            "create",
            "--name",
            &self.name,
            "--label",
            &format!("{LABEL}={}", self.execution),
            "--label",
            &format!("{OWNER}={}", self.owner),
            "--pull=never",
            "--entrypoint",
            "[\"tail\"]",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if own_network {
            arguments.push(format!("--network={}", self.network));
        }
        for (host, container, readonly) in self.layout.pairs() {
            let host = host.to_string_lossy();
            if host.contains([':', ',']) {
                return Err(format!(
                    "the directory {host} cannot be mounted into a container, since podman reads \
                     `:` and `,` in a volume as separators"
                ));
            }
            arguments.push(format!(
                "--volume={host}:{container}{}",
                if *readonly { ":ro" } else { "" }
            ));
        }
        let workdir = self.translate(workspace).unwrap_or_else(|| WORK.to_owned());
        arguments.push(format!("--workdir={workdir}"));
        arguments.extend(flags);
        arguments.extend(
            ["--", image, "-f", "/dev/null"]
                .into_iter()
                .map(str::to_owned),
        );
        Ok(arguments)
    }

    async fn login(
        &self,
        credentials: Option<&Credentials>,
        image: &str,
    ) -> Result<Option<PathBuf>, String> {
        let Some(Credentials { username, password }) = credentials else {
            return Ok(None);
        };
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.state)
            .map_err(|error| {
                format!(
                    "the directory {} for the registry credentials could not be created: {error}",
                    self.state.display()
                )
            })?;
        let file = self.state.join("auth.json");
        let registry = registry(image);
        let call = Call::new([
            OsString::from("login"),
            "--authfile".into(),
            file.clone().into_os_string(),
            format!("--username={username}").into(),
            "--password-stdin".into(),
            registry.clone().into(),
        ])
        .with_stdin(password.clone().into_bytes());
        self.run(&format!("podman could not log in to {registry}"), call)
            .await?;
        Ok(Some(file))
    }

    async fn pull(
        &self,
        job: &ContainerJob,
        credentials: Option<&Credentials>,
        image: &str,
    ) -> Result<(), String> {
        let looked = self
            .call(Call::new(["image", "exists", image]))
            .await
            .map_err(|error| format!("podman could not look for the image `{image}`: {error}"))?;
        match looked.code {
            Some(0) => return Ok(()),
            Some(1) => {}
            _ => {
                return Err(format!(
                    "podman could not look for the image `{image}`: it {}",
                    looked.failure()
                ));
            }
        }
        if job.pull_policy == PullPolicy::Never {
            return Err(format!(
                "the image `{image}` is not on this host, and the agent's `pull_policy` is \
                 `never`, so it is not pulled"
            ));
        }
        let auth = self.login(credentials, image).await?;
        let mut pull = vec![OsString::from("pull"), "--quiet".into()];
        if let Some(auth) = auth {
            pull.push("--authfile".into());
            pull.push(auth.as_os_str().to_owned());
        }
        pull.push(image.into());
        match self.engine.run(Call::new(pull)).await {
            Ok(outcome) if outcome.success() => Ok(()),
            Ok(outcome) => Err(format!(
                "the image `{image}` could not be pulled: podman {}",
                outcome.failure()
            )),
            Err(error) => Err(format!(
                "the image `{image}` could not be pulled: podman could not run: {error}"
            )),
        }
    }

    async fn libc(&self) -> Result<Libc, String> {
        let probed = self
            .run(
                "the job container could not be asked which C library it has, so the image needs \
                 `sh`",
                Call::new(["exec", &self.name, "sh", "-c", LIBC_PROBE]),
            )
            .await?;
        match probed.stdout.trim() {
            "musl" => Ok(Libc::Musl),
            "glibc" => Ok(Libc::Glibc),
            other => Err(format!(
                "the job container answered `{other}` when asked which C library it has"
            )),
        }
    }

    async fn call(&self, call: Call) -> std::io::Result<Outcome> {
        within(&self.engine, call, ANSWER_WITHIN).await
    }

    async fn run(&self, what: &str, call: Call) -> Result<Outcome, String> {
        match self.call(call).await {
            Ok(outcome) if outcome.success() => Ok(outcome),
            Ok(outcome) => Err(format!("{what}: podman {}", outcome.failure())),
            Err(error) => Err(format!("{what}: podman could not run: {error}")),
        }
    }

    pub(crate) async fn signal(&self, signal: &str) {
        let _ = within(
            &self.engine,
            Call::new([
                "exec",
                &self.name,
                "sh",
                "-c",
                &format!("kill -{signal} -1"),
            ]),
            SIGNAL_WITHIN,
        )
        .await;
    }

    async fn teardown_call(&self, call: Call) -> std::io::Result<Outcome> {
        within(&self.engine, call, TEARDOWN_WITHIN).await
    }

    pub(crate) async fn teardown(&mut self) -> Vec<String> {
        let mut faults = Faults::default();
        let filter = format!("label={LABEL}={}", self.execution);
        faults.note(
            "the job container could not be removed",
            self.teardown_call(Call::new([
                "rm",
                "--force",
                "--ignore",
                "--volumes",
                "--time",
                "0",
                &self.name,
            ]))
            .await,
        );
        for kind in Kind::ALL {
            let listed = faults.note(
                &format!("the {}s of the job could not be listed", kind.name),
                self.teardown_call(Call::new(
                    kind.list
                        .iter()
                        .copied()
                        .chain(["--filter", filter.as_str()]),
                ))
                .await,
            );
            let names: Vec<String> = listed
                .map(|listed| {
                    listed
                        .stdout
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() {
                continue;
            }
            faults.note(
                &format!("a {} of the job could not be removed", kind.name),
                self.teardown_call(Call::new(
                    kind.remove.iter().copied().map(str::to_owned).chain(names),
                ))
                .await,
            );
        }
        if let Err(error) = remove_state(&self.state) {
            faults.0.push(format!(
                "the registry credentials in {} could not be removed: {error}",
                self.state.display()
            ));
        }
        let mut faults = faults.0;
        if !faults.is_empty()
            && let Err(error) = dirty::mark(&self.root)
        {
            faults.push(format!(
                "the agent could not record in {} that the job left something behind: {error}",
                self.root.display()
            ));
        }
        faults
    }
}

pub async fn sweep(engine: &Arc<dyn Engine>, state: &Path) -> Vec<String> {
    let mut faults = Faults::default();
    let filter = format!("label={LABEL}");
    let mine = format!("label={OWNER}={}", owner(state));
    for kind in Kind::ALL {
        let listed = faults.note(
            &format!("the {}s of earlier jobs could not be listed", kind.name),
            within(
                engine,
                Call::new(kind.list.iter().copied().chain([
                    "--filter",
                    filter.as_str(),
                    "--filter",
                    mine.as_str(),
                ])),
                TEARDOWN_WITHIN,
            )
            .await,
        );
        let names: Vec<String> = listed
            .map(|listed| {
                listed
                    .stdout
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if names.is_empty() {
            continue;
        }
        faults.note(
            &format!("a {} of an earlier job could not be removed", kind.name),
            within(
                engine,
                Call::new(kind.remove.iter().copied().map(str::to_owned).chain(names)),
                TEARDOWN_WITHIN,
            )
            .await,
        );
    }
    if let Err(error) = remove_leftovers(state) {
        faults.0.push(format!(
            "the registry credentials in {} could not be removed: {error}",
            state.display()
        ));
    }
    let mut faults = faults.0;
    if faults.is_empty() {
        if let Err(error) = dirty::clear(state) {
            faults.push(format!(
                "the record in {} that a job left something behind could not be removed: {error}",
                state.display()
            ));
        }
    } else if let Err(error) = dirty::mark(state) {
        faults.push(format!(
            "the agent could not record in {} that earlier jobs left something behind: {error}",
            state.display()
        ));
    }
    faults
}

fn owner(state: &Path) -> String {
    let digest = Sha256::digest(state.as_os_str().as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn used(credentials: Option<&Credentials>) -> Result<Option<&Credentials>, String> {
    let Some(credentials) = credentials else {
        return Ok(None);
    };
    let Credentials { username, password } = credentials;
    match (username.is_empty(), password.is_empty()) {
        (true, true) => Ok(None),
        (false, false) if username.contains('\0') => {
            Err("the username of `container.credentials` holds a NUL character".to_owned())
        }
        (false, false) => Ok(Some(credentials)),
        _ => Err(
            "`container.credentials` needs a username and a password, and one of them is empty"
                .to_owned(),
        ),
    }
}

fn remove_leftovers(state: &Path) -> std::io::Result<()> {
    let entries = match fs::read_dir(state) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut failed = None;
    for entry in entries {
        let removed = entry.and_then(|entry| {
            if entry.file_name() == dirty::MARKER {
                return Ok(());
            }
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            }
        });
        if let Err(error) = removed {
            failed.get_or_insert(error);
        }
    }
    failed.map_or(Ok(()), Err)
}

struct Kind {
    name: &'static str,
    list: &'static [&'static str],
    remove: &'static [&'static str],
}

impl Kind {
    const ALL: [Kind; 3] = [
        Kind {
            name: "container",
            list: &["ps", "--all", "--quiet"],
            remove: &["rm", "--force", "--volumes", "--time", "0"],
        },
        Kind {
            name: "volume",
            list: &["volume", "ls", "--quiet"],
            remove: &["volume", "rm", "--force"],
        },
        Kind {
            name: "network",
            list: &["network", "ls", "--quiet"],
            remove: &["network", "rm", "--force"],
        },
    ];
}

#[derive(Default)]
struct Faults(Vec<String>);

impl Faults {
    fn note(&mut self, what: &str, outcome: std::io::Result<Outcome>) -> Option<Outcome> {
        match outcome {
            Ok(outcome) if outcome.success() => Some(outcome),
            Ok(outcome) => {
                self.0.push(format!("{what}: podman {}", outcome.failure()));
                None
            }
            Err(error) => {
                self.0
                    .push(format!("{what}: podman could not run: {error}"));
                None
            }
        }
    }
}

fn remove_state(state: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(state) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

const TRANSPORTS: [&str; 10] = [
    "docker-daemon:",
    "docker-archive:",
    "dir:",
    "oci:",
    "oci-archive:",
    "containers-storage:",
    "docker://",
    "tarball:",
    "sif:",
    "ostree:",
];

fn image(image: &str) -> Result<String, String> {
    let image = image.trim();
    if !reference(image) {
        return Err(format!(
            "`{}` is not the name of an image: the agent takes `[registry[:port]/]path[:tag]` \
             with an optional `@sha256:<digest>`, and no transport",
            image.escape_debug()
        ));
    }
    Ok(image.to_owned())
}

fn reference(text: &str) -> bool {
    if TRANSPORTS
        .iter()
        .any(|transport| text.starts_with(transport))
    {
        return false;
    }
    let (name, digest) = match text.split_once('@') {
        Some((name, digest)) => (name, Some(digest)),
        None => (text, None),
    };
    if digest.is_some_and(|digest| !sha256(digest)) {
        return false;
    }
    let (path, tag) = match name.rfind(':') {
        Some(colon) if !name[colon..].contains('/') => (&name[..colon], Some(&name[colon + 1..])),
        _ => (name, None),
    };
    if tag.is_some_and(|tag| !tag_text(tag)) {
        return false;
    }
    let components: Vec<&str> = path.split('/').collect();
    let (domain, rest) = match components.split_first() {
        Some((first, rest))
            if !rest.is_empty() && (first.contains(['.', ':']) || *first == "localhost") =>
        {
            (Some(*first), rest)
        }
        _ => (None, components.as_slice()),
    };
    domain.is_none_or(host) && rest.iter().all(|component| component_text(component))
}

fn sha256(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn tag_text(tag: &str) -> bool {
    let mut bytes = tag.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric() || first == b'_')
        && tag.len() <= 128
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn host(domain: &str) -> bool {
    let name = match domain.rsplit_once(':') {
        Some((name, port)) => {
            if port.is_empty() || port.len() > 5 || !port.bytes().all(|byte| byte.is_ascii_digit())
            {
                return false;
            }
            name
        }
        None => domain,
    };
    name.split('.').all(|label| {
        let bytes = label.as_bytes();
        !bytes.is_empty()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            && bytes[0] != b'-'
            && bytes[bytes.len() - 1] != b'-'
    })
}

fn component_text(component: &str) -> bool {
    let bytes = component.as_bytes();
    let plain = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let mut at = 0;
    let mut wanted = true;
    while at < bytes.len() {
        if plain(bytes[at]) {
            while at < bytes.len() && plain(bytes[at]) {
                at += 1;
            }
            wanted = false;
        } else if wanted {
            return false;
        } else {
            match bytes[at] {
                b'.' => at += 1,
                b'_' => {
                    at += if bytes.get(at + 1) == Some(&b'_') {
                        2
                    } else {
                        1
                    }
                }
                b'-' => {
                    while at < bytes.len() && bytes[at] == b'-' {
                        at += 1;
                    }
                }
                _ => return false,
            }
            wanted = true;
        }
    }
    !wanted
}

pub(crate) fn registry(image: &str) -> String {
    match image.split_once('/') {
        Some((first, _)) if first.contains(['.', ':']) || first == "localhost" => first.to_owned(),
        _ => "docker.io".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{image, registry};

    #[test]
    fn the_registry_of_an_image_is_its_first_part_when_that_looks_like_a_host() {
        for (reference, expected) in [
            ("alpine", "docker.io"),
            ("alpine:3.22", "docker.io"),
            ("acme/builder:1.2", "docker.io"),
            ("ghcr.io/acme/builder:1.2", "ghcr.io"),
            (
                "registry.example.org:5000/a/b@sha256:abc",
                "registry.example.org:5000",
            ),
            ("localhost/builder", "localhost"),
            ("localhost:5000/builder", "localhost:5000"),
        ] {
            assert_eq!(registry(reference), expected, "{reference}");
        }
    }

    #[test]
    fn an_image_is_a_registry_a_path_a_tag_and_a_digest_and_nothing_that_podman_could_read_otherwise()
     {
        let digest = format!("sha256:{}", "0123456789abcdef".repeat(4));
        assert_eq!(image(" alpine:3 ").as_deref(), Ok("alpine:3"));
        for accepted in [
            "alpine",
            "alpine:3.22",
            "node:24-alpine",
            "docker:dind",
            "acme/builder:1.2",
            "my_org/my.app-x__y:Tag_1",
            "docker.io/library/alpine",
            "ghcr.io/acme/builder:1.2",
            "registry.example.org:5000/a/b:1",
            "localhost/builder",
            "localhost:5000/builder:1",
            &format!("ghcr.io/acme/builder@{digest}"),
            &format!("ghcr.io/acme/builder:1@{digest}"),
        ] {
            assert_eq!(image(accepted).as_deref(), Ok(accepted), "{accepted}");
        }
        for refused in [
            "",
            "  ",
            "--privileged",
            "-v",
            "a b",
            "a\tb",
            "a\nb",
            "a\u{7}b",
            "docker-daemon:alpine",
            "docker-archive:/tmp/image.tar",
            "dir:/var/lib/bosun/agents/1/work/image",
            "dir:image",
            "oci:/tmp/layout:tag",
            "oci-archive:/tmp/image.tar",
            "containers-storage:alpine",
            "docker://alpine",
            "tarball:/tmp/rootfs.tar",
            "ostree:image@/repo",
            "Alpine",
            "acme//builder",
            "acme/builder:",
            "acme/builder:-1",
            "acme/Builder",
            "acme/builder_",
            "acme/_builder",
            "acme/a___b",
            "ghcr.io:port/acme/builder",
            "-ghcr.io/acme/builder",
            "ghcr.io/acme/builder@sha256:abc",
            "ghcr.io/acme/builder@md5:0123456789abcdef0123456789abcdef",
            "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "acme/builder:1:2",
            "/acme/builder",
        ] {
            assert!(image(refused).is_err(), "{refused:?}");
        }
    }
}
