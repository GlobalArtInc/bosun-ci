use bosun_proto::agent::v1::{Hello, PROTOCOL};

pub(crate) const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn admit(hello: &Hello, server: &str) -> Result<(), String> {
    if hello.protocol != PROTOCOL {
        return Err(format!(
            "the agent speaks the protocol `{}`, and this server speaks `{PROTOCOL}` only",
            hello.protocol
        ));
    }
    let (major, minor) = release(server)
        .ok_or_else(|| format!("this server's own version `{server}` is not a version"))?;
    let accepted = if minor == 0 {
        format!("{major}.0")
    } else {
        format!("{major}.{minor} and {major}.{}", minor - 1)
    };
    let refusal = || {
        format!(
            "the agent is version {}, and this server, version {server}, accepts agents of \
             version {accepted} only",
            hello.version
        )
    };
    let Some((agent_major, agent_minor)) = release(&hello.version) else {
        return Err(refusal());
    };
    if agent_major == major && (agent_minor == minor || agent_minor + 1 == minor) {
        Ok(())
    } else {
        Err(refusal())
    }
}

fn release(version: &str) -> Option<(u64, u64)> {
    let core = version
        .split_once(['-', '+'])
        .map_or(version, |(core, _)| core);
    let mut parts = core.split('.');
    let major = number(parts.next()?)?;
    let minor = number(parts.next()?)?;
    number(parts.next()?)?;
    parts.next().is_none().then_some((major, minor))
}

fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use bosun_proto::agent::v1::{Hello, PROTOCOL};

    use super::admit;

    fn hello(protocol: &str, version: &str) -> Hello {
        Hello {
            protocol: protocol.to_owned(),
            version: version.to_owned(),
            ..Hello::default()
        }
    }

    #[test]
    fn an_agent_of_the_same_or_the_previous_minor_version_is_admitted() {
        for version in ["0.5.0", "0.5.7", "0.4.2", "0.5.1-rc.1", "0.4.0+build.3"] {
            assert_eq!(
                admit(&hello(PROTOCOL, version), "0.5.3"),
                Ok(()),
                "{version}"
            );
        }
        assert_eq!(admit(&hello(PROTOCOL, "1.0.0"), "1.0.0"), Ok(()));
    }

    #[test]
    fn an_older_newer_or_unreadable_version_is_refused_naming_both_versions() {
        for version in [
            "0.3.9", "0.6.0", "1.5.0", "0.5", "0.5.x", "", "v0.5.0", "0.5.0.1",
        ] {
            assert_eq!(
                admit(&hello(PROTOCOL, version), "0.5.3"),
                Err(format!(
                    "the agent is version {version}, and this server, version 0.5.3, accepts \
                     agents of version 0.5 and 0.4 only"
                )),
                "{version}"
            );
        }
        assert_eq!(
            admit(&hello(PROTOCOL, "0.9.0"), "1.0.2"),
            Err(
                "the agent is version 0.9.0, and this server, version 1.0.2, accepts agents of \
                 version 1.0 only"
                    .to_owned()
            )
        );
    }

    #[test]
    fn another_protocol_is_refused_naming_both_protocols() {
        for protocol in ["bosun.agent.v2", "bosun.agent.v0", ""] {
            assert_eq!(
                admit(&hello(protocol, "0.5.3"), "0.5.3"),
                Err(format!(
                    "the agent speaks the protocol `{protocol}`, and this server speaks \
                     `bosun.agent.v1` only"
                ))
            );
        }
    }
}
