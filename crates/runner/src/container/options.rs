use std::fmt;

use super::volumes;

const WEAKENS: &str = "weakens the isolation of the container";
const SHARES: &str = "shares a namespace with the host or with another container";
const READS: &str = "reads a file of the host into the container";
const AGENT: &str = "is chosen by the agent";
const UNKNOWN: &str = "is not an option that the agent passes to podman";

const SECURITY: &[&str] = &[
    "no-new-privileges",
    "no-new-privileges:true",
    "no-new-privileges=true",
];

#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy<'a> {
    pub(crate) allow_privileged: bool,
    pub(crate) untrusted: bool,
    pub(crate) workspace: &'a std::path::Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal(String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Take {
    Switch,
    Value,
}

#[derive(Clone, Copy)]
enum Rule {
    Safe,
    Env,
    Namespace(&'static [&'static str]),
    Publish,
    Security,
    Volume,
    Mount,
    Privileged(&'static str),
    Reserved,
}

struct Flag {
    name: &'static str,
    short: Option<char>,
    take: Take,
    rule: Rule,
}

const fn safe(name: &'static str, short: Option<char>, take: Take) -> Flag {
    Flag {
        name,
        short,
        take,
        rule: Rule::Safe,
    }
}

const fn privileged(
    name: &'static str,
    short: Option<char>,
    take: Take,
    why: &'static str,
) -> Flag {
    Flag {
        name,
        short,
        take,
        rule: Rule::Privileged(why),
    }
}

const fn reserved(name: &'static str, short: Option<char>, take: Take) -> Flag {
    Flag {
        name,
        short,
        take,
        rule: Rule::Reserved,
    }
}

const fn namespace(name: &'static str, allowed: &'static [&'static str]) -> Flag {
    Flag {
        name,
        short: None,
        take: Take::Value,
        rule: Rule::Namespace(allowed),
    }
}

use Take::{Switch, Value};

const FLAGS: &[Flag] = &[
    safe("add-host", None, Value),
    safe("blkio-weight", None, Value),
    safe("cap-drop", None, Value),
    safe("cpu-period", None, Value),
    safe("cpu-quota", None, Value),
    safe("cpu-shares", None, Value),
    safe("cpus", None, Value),
    safe("cpuset-cpus", None, Value),
    safe("cpuset-mems", None, Value),
    safe("dns", None, Value),
    safe("dns-option", None, Value),
    safe("dns-search", None, Value),
    Flag {
        name: "env",
        short: Some('e'),
        take: Value,
        rule: Rule::Env,
    },
    safe("expose", None, Value),
    safe("health-cmd", None, Value),
    safe("health-interval", None, Value),
    safe("health-on-failure", None, Value),
    safe("health-retries", None, Value),
    safe("health-start-period", None, Value),
    safe("health-startup-cmd", None, Value),
    safe("health-startup-interval", None, Value),
    safe("health-startup-retries", None, Value),
    safe("health-startup-success", None, Value),
    safe("health-startup-timeout", None, Value),
    safe("health-timeout", None, Value),
    safe("hostname", Some('h'), Value),
    safe("init", None, Switch),
    safe("memory", Some('m'), Value),
    safe("memory-reservation", None, Value),
    safe("memory-swap", None, Value),
    safe("memory-swappiness", None, Value),
    safe("no-healthcheck", None, Switch),
    safe("no-hostname", None, Switch),
    safe("no-hosts", None, Switch),
    safe("oom-kill-disable", None, Switch),
    safe("oom-score-adj", None, Value),
    safe("pids-limit", None, Value),
    Flag {
        name: "publish",
        short: Some('p'),
        take: Value,
        rule: Rule::Publish,
    },
    Flag {
        name: "publish-all",
        short: Some('P'),
        take: Switch,
        rule: Rule::Publish,
    },
    safe("read-only", None, Switch),
    safe("read-only-tmpfs", None, Switch),
    safe("shm-size", None, Value),
    safe("stop-signal", None, Value),
    safe("stop-timeout", None, Value),
    safe("tmpfs", None, Value),
    safe("ulimit", None, Value),
    safe("unsetenv", None, Value),
    safe("unsetenv-all", None, Switch),
    safe("user", Some('u'), Value),
    safe("workdir", Some('w'), Value),
    namespace("pid", &["private"]),
    namespace("ipc", &["private", "none", "shareable"]),
    namespace("uts", &["private"]),
    namespace("userns", &["keep-id", "nomap"]),
    namespace("cgroupns", &["private"]),
    Flag {
        name: "security-opt",
        short: None,
        take: Value,
        rule: Rule::Security,
    },
    Flag {
        name: "volume",
        short: Some('v'),
        take: Value,
        rule: Rule::Volume,
    },
    Flag {
        name: "mount",
        short: None,
        take: Value,
        rule: Rule::Mount,
    },
    privileged("privileged", None, Switch, WEAKENS),
    privileged("cap-add", None, Value, WEAKENS),
    privileged("device", None, Value, WEAKENS),
    privileged("device-cgroup-rule", None, Value, WEAKENS),
    privileged("device-read-bps", None, Value, WEAKENS),
    privileged("device-read-iops", None, Value, WEAKENS),
    privileged("device-write-bps", None, Value, WEAKENS),
    privileged("device-write-iops", None, Value, WEAKENS),
    privileged("blkio-weight-device", None, Value, WEAKENS),
    privileged("gpus", None, Value, WEAKENS),
    privileged("systemd", None, Value, WEAKENS),
    privileged("sysctl", None, Value, WEAKENS),
    privileged("group-add", None, Value, WEAKENS),
    privileged("cgroup-parent", None, Value, WEAKENS),
    privileged("cgroup-conf", None, Value, WEAKENS),
    privileged("cgroups", None, Value, WEAKENS),
    privileged("volumes-from", None, Value, WEAKENS),
    privileged("network", None, Value, AGENT),
    privileged("net", None, Value, AGENT),
    privileged("env-file", None, Value, READS),
    privileged("env-host", None, Switch, READS),
    privileged("secret", None, Value, READS),
    privileged("authfile", None, Value, READS),
    privileged("cert-dir", None, Value, READS),
    privileged("decryption-key", None, Value, READS),
    privileged("hooks-dir", None, Value, READS),
    privileged("init-path", None, Value, READS),
    privileged("log-driver", None, Value, READS),
    privileged("log-opt", None, Value, READS),
    privileged("health-log-destination", None, Value, READS),
    privileged("rootfs", None, Switch, READS),
    reserved("name", None, Value),
    reserved("entrypoint", None, Value),
    reserved("label", Some('l'), Value),
    reserved("label-file", None, Value),
    reserved("pull", None, Value),
    reserved("rm", None, Switch),
    reserved("replace", None, Switch),
    reserved("cidfile", None, Value),
    reserved("pod", None, Value),
    reserved("pod-id-file", None, Value),
];

fn long(name: &str) -> Option<&'static Flag> {
    FLAGS.iter().find(|flag| flag.name == name)
}

fn short(letter: char) -> Option<&'static Flag> {
    FLAGS.iter().find(|flag| flag.short == Some(letter))
}

pub(crate) fn unreadable(value: &str) -> Option<String> {
    value.chars().find_map(|character| {
        if character.is_control() {
            Some(format!("the control character U+{:04X}", character as u32))
        } else if character.is_whitespace() && !character.is_ascii() {
            Some(format!(
                "the space U+{:04X}, which is not an ASCII one",
                character as u32
            ))
        } else {
            None
        }
    })
}

pub(crate) fn arguments(text: &str, policy: &Policy<'_>) -> Result<Vec<String>, Refusal> {
    let tokens = split(text).map_err(|reason| {
        Refusal(format!(
            "the options of `container` cannot be read: {reason}"
        ))
    })?;
    if let Some(found) = tokens.iter().find_map(|token| unreadable(token)) {
        return Err(Refusal(format!(
            "the options of `container` hold {found}, which podman may read differently from the \
             agent"
        )));
    }
    let mut passed = Vec::new();
    let mut tokens = tokens.into_iter().peekable();
    while let Some(token) = tokens.next() {
        if token == "--" {
            return Err(Refusal(
                "the options of `container` hold `--`, after which the agent sets the image itself"
                    .to_owned(),
            ));
        }
        if let Some(rest) = token.strip_prefix("--") {
            let (name, attached) = match rest.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (rest, None),
            };
            match long(name) {
                Some(flag) => {
                    let (value, written) = take(flag, &token, attached, &mut tokens)?;
                    judge(flag, value.as_deref(), &written, policy)?;
                    passed.push(match value {
                        Some(value) => format!("--{name}={value}"),
                        None => format!("--{name}"),
                    });
                }
                None => {
                    unknown(&token, name, attached, &mut tokens, policy)?;
                    passed.push(token);
                }
            }
        } else if token.len() > 1 && token.starts_with('-') {
            let letters: Vec<char> = token[1..].chars().collect();
            let mut position = 0;
            while position < letters.len() {
                let letter = letters[position];
                let Some(flag) = short(letter) else {
                    return Err(Refusal(format!(
                        "the option `-{letter}` of `container` is not one that the agent knows, \
                         and `{token}` cannot be passed to podman"
                    )));
                };
                position += 1;
                let rest: String = letters[position..].iter().collect();
                let (value, written) = match flag.take {
                    Take::Switch => (None, format!("-{letter}")),
                    Take::Value if !rest.is_empty() => {
                        position = letters.len();
                        let value = rest.strip_prefix('=').unwrap_or(&rest).to_owned();
                        (Some(value.clone()), format!("-{letter} {value}"))
                    }
                    Take::Value => {
                        let Some(value) = tokens.next() else {
                            return Err(Refusal(format!(
                                "the option `-{letter}` of `container` needs a value"
                            )));
                        };
                        (Some(value.clone()), format!("-{letter} {value}"))
                    }
                };
                judge(flag, value.as_deref(), &written, policy)?;
                passed.push(match value {
                    Some(value) => format!("--{}={value}", flag.name),
                    None => format!("--{}", flag.name),
                });
            }
        } else {
            return Err(Refusal(format!(
                "`{token}` in the options of `container` is not an option: the agent adds the \
                 image and the command itself"
            )));
        }
    }
    Ok(passed)
}

fn take(
    flag: &Flag,
    token: &str,
    attached: Option<String>,
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
) -> Result<(Option<String>, String), Refusal> {
    match (flag.take, attached) {
        (Take::Switch, None) => Ok((None, token.to_owned())),
        (Take::Switch, Some(value)) => {
            if matches!(
                value.as_str(),
                "true"
                    | "false"
                    | "1"
                    | "0"
                    | "t"
                    | "f"
                    | "T"
                    | "F"
                    | "TRUE"
                    | "FALSE"
                    | "True"
                    | "False"
            ) {
                Ok((Some(value), token.to_owned()))
            } else {
                Err(Refusal(format!(
                    "the option `{token}` of `container` takes `true` or `false`, not `{value}`"
                )))
            }
        }
        (Take::Value, Some(value)) => Ok((Some(value), token.to_owned())),
        (Take::Value, None) => match tokens.next() {
            Some(value) => Ok((Some(value.clone()), format!("{token} {value}"))),
            None => Err(Refusal(format!(
                "the option `{token}` of `container` needs a value"
            ))),
        },
    }
}

fn unknown(
    token: &str,
    name: &str,
    attached: Option<String>,
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
    policy: &Policy<'_>,
) -> Result<(), Refusal> {
    if !policy.allow_privileged {
        return Err(Refusal(format!(
            "the option `{token}` of `container` {UNKNOWN}, and the pool does not set \
             `allow_privileged`"
        )));
    }
    if name.is_empty() {
        return Err(Refusal(format!(
            "`{token}` in the options of `container` is not an option"
        )));
    }
    if attached.is_none()
        && let Some(next) = tokens.peek()
        && !next.starts_with('-')
    {
        return Err(Refusal(format!(
            "the option `{token}` of `container` is not one that the agent knows, so it cannot \
             tell whether `{next}` is its value: write `{token}={next}`"
        )));
    }
    Ok(())
}

fn judge(
    flag: &Flag,
    value: Option<&str>,
    written: &str,
    policy: &Policy<'_>,
) -> Result<(), Refusal> {
    let value = value.unwrap_or_default();
    let malformed = match flag.rule {
        Rule::Volume => volumes::malformed(value, false),
        Rule::Mount => volumes::malformed(value, true),
        _ => None,
    };
    if let Some(why) = malformed {
        return Err(Refusal(format!(
            "the option `{written}` of `container` {why}, which podman may read differently from \
             the agent"
        )));
    }
    let verdict: Result<(), &str> = match flag.rule {
        Rule::Safe => Ok(()),
        Rule::Reserved => {
            return Err(Refusal(format!(
                "the option `{written}` of `container` is chosen by the agent and cannot be set"
            )));
        }
        Rule::Privileged(why) => Err(why),
        Rule::Env
            if value
                .split_once('=')
                .is_some_and(|(name, _)| !name.is_empty()) =>
        {
            Ok(())
        }
        Rule::Env => Err(READS),
        Rule::Namespace(allowed) if allowed.contains(&value) => Ok(()),
        Rule::Namespace(_) => Err(SHARES),
        Rule::Publish if policy.untrusted => {
            return Err(Refusal(format!(
                "the option `{written}` of `container` publishes a port on the host, which a \
                 pool of untrusted code never does"
            )));
        }
        Rule::Publish => Ok(()),
        Rule::Security if SECURITY.contains(&value) => Ok(()),
        Rule::Security => Err(WEAKENS),
        Rule::Volume => match volumes::volume(value, policy) {
            Ok(()) => Ok(()),
            Err(reason) => return gate(written, &reason, policy),
        },
        Rule::Mount => match volumes::mount(value, policy) {
            Ok(()) => Ok(()),
            Err(reason) => return gate(written, &reason, policy),
        },
    };
    match verdict {
        Ok(()) => Ok(()),
        Err(why) => gate(written, why, policy),
    }
}

fn gate(written: &str, why: &str, policy: &Policy<'_>) -> Result<(), Refusal> {
    if policy.allow_privileged {
        Ok(())
    } else {
        Err(Refusal(format!(
            "the option `{written}` of `container` {why}, and the pool does not set \
             `allow_privileged`"
        )))
    }
}

pub(crate) fn split(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        match character {
            '\0' => return Err("it holds a NUL character".to_owned()),
            '\'' => {
                started = true;
                loop {
                    match characters.next() {
                        Some('\'') => break,
                        Some(inside) => word.push(inside),
                        None => return Err("a single quote is not closed".to_owned()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match characters.next() {
                        Some('"') => break,
                        Some('\\') => match characters.next() {
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => word.push(escaped),
                            Some('\n') => {}
                            Some(other) => {
                                word.push('\\');
                                word.push(other);
                            }
                            None => return Err("a double quote is not closed".to_owned()),
                        },
                        Some(inside) => word.push(inside),
                        None => return Err("a double quote is not closed".to_owned()),
                    }
                }
            }
            '\\' => match characters.next() {
                Some('\n') => {}
                Some(escaped) => {
                    started = true;
                    word.push(escaped);
                }
                None => return Err("it ends with a backslash".to_owned()),
            },
            space if space.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            other => {
                started = true;
                word.push(other);
            }
        }
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{Policy, arguments, split};

    fn policy(workspace: &Path, allow_privileged: bool, untrusted: bool) -> Policy<'_> {
        Policy {
            allow_privileged,
            untrusted,
            workspace,
        }
    }

    fn refusal(text: &str, workspace: &Path) -> String {
        match arguments(text, &policy(workspace, false, false)) {
            Ok(passed) => panic!("`{text}` was passed on as {passed:?}"),
            Err(refusal) => refusal.to_string(),
        }
    }

    #[test]
    fn every_flag_that_weakens_isolation_is_refused_in_both_spellings_naming_the_flag() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let workspace = fs::canonicalize(workspace.path()).expect("a path");
        for (text, flag) in [
            ("--privileged", "--privileged"),
            ("--privileged=true", "--privileged"),
            ("--cap-add=SYS_ADMIN", "--cap-add"),
            ("--cap-add SYS_ADMIN", "--cap-add"),
            ("--device=/dev/fuse", "--device"),
            ("--device /dev/fuse", "--device"),
            ("--pid=host", "--pid=host"),
            ("--pid host", "--pid host"),
            ("--pid=container:abc", "--pid=container:abc"),
            ("--network=host", "--network=host"),
            ("--network host", "--network host"),
            ("--net=host", "--net=host"),
            ("--net host", "--net host"),
            ("--ipc=host", "--ipc=host"),
            ("--ipc host", "--ipc host"),
            ("--uts=host", "--uts=host"),
            ("--uts host", "--uts host"),
            ("--userns=host", "--userns=host"),
            ("--userns host", "--userns host"),
            ("--cgroupns=host", "--cgroupns=host"),
            ("--cgroupns host", "--cgroupns host"),
            ("--security-opt=seccomp=unconfined", "--security-opt"),
            ("--security-opt seccomp=unconfined", "--security-opt"),
            ("--security-opt apparmor=unconfined", "--security-opt"),
            ("--security-opt label=disable", "--security-opt"),
            ("--security-opt no-new-privileges:false", "--security-opt"),
            ("--volumes-from=other", "--volumes-from"),
            ("--volumes-from other", "--volumes-from"),
            ("-v /etc:/etc", "-v /etc:/etc"),
            ("-v/etc:/etc", "-v /etc:/etc"),
            ("-v=/etc:/etc", "-v /etc:/etc"),
            ("--volume=/etc:/etc", "--volume=/etc:/etc"),
            ("--volume /etc:/etc:ro", "--volume /etc:/etc:ro"),
            (
                "--mount type=bind,src=/etc,dst=/etc",
                "--mount type=bind,src=/etc,dst=/etc",
            ),
            (
                "--mount=type=bind,source=/etc,target=/etc",
                "--mount=type=bind,source=/etc",
            ),
            ("--mount type=image,src=x,dst=/x", "--mount"),
            (
                "--mount type=bind,src=/etc,dst=/etc,bind-propagation=shared",
                "--mount",
            ),
            ("--env-file /etc/passwd", "--env-file"),
            ("-e SECRET_OF_THE_AGENT", "-e SECRET_OF_THE_AGENT"),
            ("--group-add keep-groups", "--group-add"),
            ("--sysctl net.ipv4.ip_forward=1", "--sysctl"),
        ] {
            let message = refusal(text, &workspace);
            assert!(message.contains(&format!("`{flag}")), "{text}: {message}");
            assert!(message.contains("`allow_privileged`"), "{text}: {message}");
        }
    }

    #[test]
    fn what_the_agent_cannot_tell_or_chooses_itself_is_refused_even_in_a_privileged_pool() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let workspace = fs::canonicalize(workspace.path()).expect("a path");
        for text in [
            "--frobnicate",
            "--Privileged",
            "--PRIVILEGED",
            "-Z",
            "alpine sh",
            "--",
            "-",
            "--cpus",
            "--hostname 'unclosed",
            "--hostname \"unclosed",
            "--hostname trailing\\",
            "--read-only=sometimes",
            "--cap-add",
        ] {
            let message = refusal(text, &workspace);
            assert!(!message.is_empty(), "{text}");
        }
        for text in [
            "--name=mine",
            "--name mine",
            "--entrypoint sh",
            "-l key=value",
            "--label=key=value",
            "--pull=always",
            "--rm",
            "--cidfile=/tmp/x",
        ] {
            let privileged = policy(&workspace, true, false);
            let message = arguments(text, &privileged)
                .expect_err(&format!("`{text}` was passed on in a privileged pool"));
            assert!(
                message.to_string().contains("chosen by the agent"),
                "{text}: {message}"
            );
        }
        let privileged = policy(&workspace, true, false);
        assert!(arguments("--foo bar", &privileged).is_err());
        assert!(arguments("-Z", &privileged).is_err());
    }

    #[test]
    fn a_privileged_pool_passes_what_a_trusted_pool_refuses() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let workspace = fs::canonicalize(workspace.path()).expect("a path");
        let privileged = policy(&workspace, true, false);
        for (text, passed) in [
            ("--privileged", vec!["--privileged"]),
            ("--cap-add SYS_ADMIN", vec!["--cap-add=SYS_ADMIN"]),
            ("--device /dev/fuse", vec!["--device=/dev/fuse"]),
            ("--pid=host", vec!["--pid=host"]),
            ("--network=host", vec!["--network=host"]),
            ("--net host", vec!["--net=host"]),
            ("-v /etc:/etc:ro", vec!["--volume=/etc:/etc:ro"]),
            ("--foo=bar --baz", vec!["--foo=bar", "--baz"]),
        ] {
            assert_eq!(
                arguments(text, &privileged),
                Ok(passed.into_iter().map(str::to_owned).collect()),
                "{text}"
            );
        }
    }

    #[test]
    fn known_safe_options_pass_in_the_forms_podman_reads_and_come_out_in_one_form() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let workspace = fs::canonicalize(workspace.path()).expect("a path");
        fs::create_dir(workspace.join("data")).expect("a directory");
        let inside = workspace.join("data");
        let inside = inside.display();
        let trusted = policy(&workspace, false, false);
        for (text, passed) in [
            ("", vec![]),
            ("--cpus 2 --memory=1g", vec!["--cpus=2", "--memory=1g"]),
            (
                "-m 1g -u 1000:1000 -h box",
                vec!["--memory=1g", "--user=1000:1000", "--hostname=box"],
            ),
            (
                "-e A=b --env C=d --env=E=f=g",
                vec!["--env=A=b", "--env=C=d", "--env=E=f=g"],
            ),
            ("-eA=b", vec!["--env=A=b"]),
            (
                "--health-cmd \"curl -f 'http://localhost'\" --health-retries 3",
                vec![
                    "--health-cmd=curl -f 'http://localhost'",
                    "--health-retries=3",
                ],
            ),
            (
                "--pid=private --ipc none --uts private --cgroupns private",
                vec![
                    "--pid=private",
                    "--ipc=none",
                    "--uts=private",
                    "--cgroupns=private",
                ],
            ),
            ("--userns keep-id", vec!["--userns=keep-id"]),
            (
                "--security-opt no-new-privileges",
                vec!["--security-opt=no-new-privileges"],
            ),
            (
                "--init --read-only=false -P -p 8080:80",
                vec![
                    "--init",
                    "--read-only=false",
                    "--publish-all",
                    "--publish=8080:80",
                ],
            ),
            ("-Pp 80", vec!["--publish-all", "--publish=80"]),
            (
                "--cap-drop ALL --tmpfs /run",
                vec!["--cap-drop=ALL", "--tmpfs=/run"],
            ),
            ("-v data:/data", vec!["--volume=data:/data"]),
            ("-v /data", vec!["--volume=/data"]),
            (
                "--mount type=tmpfs,dst=/scratch,tmpfs-size=64m",
                vec!["--mount=type=tmpfs,dst=/scratch,tmpfs-size=64m"],
            ),
            (
                "--mount type=volume,src=cache,dst=/cache,ro",
                vec!["--mount=type=volume,src=cache,dst=/cache,ro"],
            ),
        ] {
            assert_eq!(
                arguments(text, &trusted),
                Ok(passed.into_iter().map(str::to_owned).collect()),
                "{text}"
            );
        }
        let in_workspace = format!("-v {inside}:/data:ro");
        assert_eq!(
            arguments(&in_workspace, &trusted),
            Ok(vec![format!("--volume={inside}:/data:ro")])
        );
        let mounted = format!("--mount type=bind,src={inside},dst=/data");
        assert_eq!(
            arguments(&mounted, &trusted),
            Ok(vec![format!("--mount=type=bind,src={inside},dst=/data")])
        );
    }

    #[test]
    fn a_volume_leaves_the_workspace_through_dots_links_relative_paths_and_missing_files() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let workspace = fs::canonicalize(workspace.path()).expect("a path");
        let elsewhere = tempfile::tempdir().expect("a directory");
        let elsewhere = fs::canonicalize(elsewhere.path()).expect("a path");
        std::os::unix::fs::symlink(&elsewhere, workspace.join("link")).expect("a link");
        let root = workspace.display();
        for (text, reason) in [
            (format!("-v {root}/../:/x"), "contains `..`"),
            (format!("-v {root}/link:/x"), "outside the workspace"),
            (format!("-v {root}/absent:/x"), "cannot be checked"),
            ("-v ./data:/x".to_owned(), "relative path"),
            ("-v ~/data:/x".to_owned(), "relative path"),
            ("-v /:/host".to_owned(), "outside the workspace"),
            ("-v data:ro".to_owned(), "cannot read"),
            ("-v a:b:c:d".to_owned(), "cannot read"),
            ("-v data:/x:Z,U".to_owned(), "volume option `U`"),
            ("-v data:/x:shared".to_owned(), "volume option `shared`"),
            ("-v data:relative".to_owned(), "cannot read"),
            (
                format!("--mount type=bind,src={root}/link,dst=/x"),
                "outside the workspace",
            ),
            ("--mount type=bind,dst=/x".to_owned(), "without a source"),
            (
                "--mount type=bind,src=/etc".to_owned(),
                "without a destination",
            ),
            (
                "--mount type=volume,src=data,dst=/x,idmap".to_owned(),
                "does not allow",
            ),
            (
                "--mount type=volume,src=data,dst=/x,chown=true".to_owned(),
                "does not allow",
            ),
            ("--mount type=devpts,dst=/x".to_owned(), "does not allow"),
        ] {
            let message = refusal(&text, &workspace);
            assert!(message.contains(reason), "{text}: {message}");
        }
    }

    #[test]
    fn what_podman_could_read_differently_from_the_agent_is_refused_even_in_a_privileged_pool() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let workspace = fs::canonicalize(workspace.path()).expect("a path");
        let root = workspace.display();
        let privileged = policy(&workspace, true, false);
        for (text, reason) in [
            (
                format!("--mount \"type=bind,src=/etc,dst=/x\n,src={root}/ok\""),
                "control character U+000A",
            ),
            (
                format!("--mount \"type=bind,src=/etc,dst=/x\r,src={root}\""),
                "control character U+000D",
            ),
            (
                format!("--mount \"type=bind,src={root},dst=/x\t\""),
                "control character U+0009",
            ),
            (
                "--hostname \"box\u{7f}\"".to_owned(),
                "control character U+007F",
            ),
            (
                "--hostname \"box\u{1b}[0m\"".to_owned(),
                "control character U+001B",
            ),
            (
                "--hostname \"box\u{85}\"".to_owned(),
                "control character U+0085",
            ),
            ("--hostname \"a\u{a0}b\"".to_owned(), "space U+00A0"),
            ("--hostname \"a\u{2003}b\"".to_owned(), "space U+2003"),
            ("--hostname \"a\u{3000}b\"".to_owned(), "space U+3000"),
            ("-v \"data\n:/x\"".to_owned(), "control character U+000A"),
            (
                "--mount type=tmpfs,dst=/a,dst=/b".to_owned(),
                "`dst` more than once",
            ),
            (
                "--mount type=tmpfs,dst=/a,target=/b".to_owned(),
                "`dst` more than once",
            ),
            (
                format!("--mount type=bind,src={root},source=/etc,dst=/x"),
                "`src` more than once",
            ),
            (
                "--mount type=tmpfs,type=bind,dst=/a".to_owned(),
                "`type` more than once",
            ),
            (
                "--mount type=tmpfs,dst=/a,ro,readonly".to_owned(),
                "`ro` more than once",
            ),
            (
                "--mount 'type=bind,\"src=/etc\",dst=/x'".to_owned(),
                "holds `\"`",
            ),
            ("--mount \"type=tmpfs,dst=/it's\"".to_owned(), "holds `'`"),
            ("--mount type=tmpfs,dst=/a\\\\b".to_owned(), "holds `\\`"),
            ("-v 'a\\b:/x'".to_owned(), "holds `\\`"),
            ("-v \"data':/x\"".to_owned(), "holds `'`"),
        ] {
            let message = refusal(&text, &workspace);
            assert!(message.contains(reason), "{text:?}: {message}");
            let hard = arguments(&text, &privileged)
                .expect_err(&format!("{text:?} was passed on in a privileged pool"))
                .to_string();
            assert!(hard.contains(reason), "{text:?}: {hard}");
        }
    }

    #[test]
    fn a_pool_of_untrusted_code_publishes_no_port_and_the_flags_with_a_doubtful_reach_are_unknown()
    {
        let workspace = tempfile::tempdir().expect("a workspace");
        let workspace = fs::canonicalize(workspace.path()).expect("a path");
        let untrusted = policy(&workspace, false, true);
        for text in ["-p 8080:80", "--publish=80", "-P", "--publish-all"] {
            let refused = arguments(text, &untrusted)
                .expect_err("a refusal")
                .to_string();
            assert!(refused.contains("publishes a port"), "{text}: {refused}");
        }
        let trusted = policy(&workspace, false, false);
        assert!(arguments("-p 8080:80", &trusted).is_ok());
        for text in [
            "--annotation run.oci.keep_original_groups=1",
            "--annotation=run.oci.handler=krun",
            "--tz=../../etc/passwd",
            "--umask 0",
            "--platform linux/arm64",
            "--arch arm64",
            "--os linux",
            "--variant v8",
        ] {
            let refused = arguments(text, &trusted)
                .expect_err("a refusal")
                .to_string();
            assert!(
                refused.contains("is not an option that the agent passes to podman"),
                "{text}: {refused}"
            );
        }
    }

    #[test]
    fn an_untrusted_pool_refuses_the_named_volumes_that_would_keep_data_between_jobs() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let workspace = fs::canonicalize(workspace.path()).expect("a path");
        let untrusted = policy(&workspace, false, true);
        for text in ["-v data:/x", "--mount type=volume,src=data,dst=/x"] {
            let refused = arguments(text, &untrusted)
                .expect_err("a refusal")
                .to_string();
            assert!(
                refused.contains("keep data between jobs"),
                "{text}: {refused}"
            );
        }
        assert!(arguments("-v /x", &untrusted).is_ok());
    }

    #[test]
    fn options_are_split_like_a_shell_without_expanding_anything() {
        assert_eq!(
            split("--a=1  --b 'two words' \"three \\\"words\\\" $HOME\" four\\ five")
                .expect("words"),
            vec![
                "--a=1",
                "--b",
                "two words",
                "three \"words\" $HOME",
                "four five"
            ]
        );
        assert_eq!(split("").expect("words"), Vec::<String>::new());
        assert_eq!(split("''").expect("words"), vec![String::new()]);
        assert!(split("a\0b").is_err());
    }
}
