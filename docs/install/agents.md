# Adding agents

An agent runs jobs on a Linux host that can reach the server's address; it opens the connection
itself, so the host needs no inbound access. One agent runs one job at a time; a host runs as many
instances as it should run jobs at once.

## One command

Create a registration token for a pool. The installation starts with the pool `default`:

```sh
bosun pool token create default --reusable --expires 7d
```

The token alone goes to standard output; standard error says which pool it registers in until
when, followed by the command to run on the agent host:

```sh
curl -fsSL https://ci.example.org/install-agent.sh | sudo sh -s -- --token brt_…
```

The server serves `install-agent.sh`, and the agent binaries of its own version at
`/_runtime/agent/linux-x86_64` and `/_runtime/agent/linux-aarch64`, so the agent always matches the
server. The script takes:

| Option | Meaning |
|--------|---------|
| `--token <token>` | the registration token, required |
| `--instances <n>` | how many agents the host runs, 1 by default; more than one needs a token created with `--reusable` |
| `--label <label>` | a label beyond the automatic ones (`self-hosted`, `linux`, `x64` or `arm64`, `shell`); repeat it for more |

## What the script does

On a Debian or Ubuntu host with systemd, as root, the script:

- downloads the agent for the host's architecture from the server, checks that it reports the
  server's version, and installs it as `/usr/local/bin/bosun-agent`;
- creates the group `bosun-cache` and the directories `/etc/bosun`, `/var/lib/bosun/agents` and
  `/var/lib/bosun/cache`, the last one of the group with the setgid bit;
- installs the template unit `bosun-agent@.service`, which runs instance `<n>` as the user
  `bosun-agent-<n>` with `bosun-agent run --config /etc/bosun/agent-<n>.toml` and delegates its
  cgroup to the agent, so that each job gets a cgroup of its own;
- for each instance, creates the user `bosun-agent-<n>` in `bosun-cache` with subordinate uid and
  gid ranges and lingering, which rootless podman needs, and its directory
  `/var/lib/bosun/agents/<n>`; registers it with `bosun-agent register`, which writes
  `/etc/bosun/agent-<n>.toml` readable only by that user; and enables and starts
  `bosun-agent@<n>.service`;
- says which prerequisites of the `podman` capability (podman 4.4 or newer, cgroups v2,
  netavark) the host lacks. The agents of this version run jobs on the host, with the `shell`
  capability.

`journalctl -u bosun-agent@1` shows the log of instance 1, and `bosun agent list` shows the agents
the server knows, with whether they are online.

## Upgrading and changing agents

Running the same command again replaces the binary with the server's version and restarts every
instance; instances registered already keep their configuration and are not registered again.
Until agents can be drained, a restart cancels the job an instance runs, so upgrade while the
agents are idle. A larger `--instances` adds instances; labels of a registered instance are the
`labels` of its configuration file, followed by a restart of its unit.
