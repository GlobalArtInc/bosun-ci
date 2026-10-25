# Installing Bosun

Bosun installs on any Linux server the same way: one container image, `globalartltd/bosun` on
Docker Hub for `linux/amd64` and `linux/arm64`, and a Postgres. The server hands agents everything
they need, so an agent host downloads nothing from anywhere else.

- [compose.md](compose.md): Bosun and Postgres together with `docker compose` or `podman compose`
- [quadlet.md](quadlet.md): Bosun as a systemd service under rootless podman
- [reverse-proxy.md](reverse-proxy.md): TLS in front of Bosun with nginx or Caddy
- [agents.md](agents.md): adding agents to a pool with one command

## What a host needs

- Linux with a container runtime: podman, docker, or Kubernetes.
- Postgres 15 or newer, with a database and a user that owns it.
- A reverse proxy that terminates TLS and speaks HTTP/2 to Bosun for the agent stream
  ([reverse-proxy.md](reverse-proxy.md)). On a closed network the agents may talk to port 8080
  directly over plain HTTP.

## Tags

| Tag | Meaning |
|-----|---------|
| `<YYYYMMDDHHMMSS>-<sha7>` | one build of `main`; production pins one of these |
| `edge` | the newest build of `main` |
| `<version>` | a release, built from a tag `v<version>` |
| `latest` | the newest release |

Until the first release is tagged, `latest` does not exist yet: use `edge` or a timestamp tag in
the commands below.

## Quick start on one host

The server needs two things: the address of its database and a volume for its data directory.

```sh
mkdir -p /srv/bosun/data
chown 210:210 /srv/bosun/data
podman run -d --name bosun -p 8080:8080 \
  -v /srv/bosun/data:/bosun-data:Z \
  -e BOSUN_DATABASE_URL=postgres://bosun:change-me@db.example.org:5432/bosun \
  docker.io/globalartltd/bosun:latest
podman exec bosun cat /bosun-data/admin.token
```

With docker, `docker run` and `docker exec` take the same arguments. A named volume
(`-v bosun-data:/bosun-data`) needs no `chown`, because the runtime copies the image's ownership
into it on first use.

On its first start against a database without users the server:

- brings the schema up to date;
- generates the KEK, the key every secret is encrypted under, into `keys/kek` and logs a warning
  that asks for a backup of it;
- uses the directory `store/` as its object store;
- creates the owner `admin` and writes its personal token to `admin.token` and its generated
  password to `admin.password`, both readable only by the server's user (uid 210). The token
  expires after a year. The first sign-in with the password asks for a new one, and setting it
  deletes both files; the token stays valid.

Later starts with the same volume reuse all of it and write nothing new. `bosun-server check`,
which the image runs as its health check, exits with 0 once the server answers `/readyz`.

## Signing in

The CLI `bosun` reads the token from standard input:

```sh
podman exec bosun cat /bosun-data/admin.token | bosun login --server https://ci.example.org
bosun agent list
```

Until releases attach binaries, build the CLI from a checkout of the repository: `cargo build
--release -p bosun-cli` with Rust installed, or, for a Linux host without Rust,
`docker build --target cli --output type=local,dest=. .`, which leaves a static `bosun` in the
current directory.

## The data directory

`/bosun-data` in the image, `/var/lib/bosun` for a binary on a host (`server.data_dir`,
`BOSUN_SERVER_DATA_DIR`):

| Path | Content |
|------|---------|
| `etc/bosun.toml` | the configuration file, read when no `--config` is given and `/etc/bosun/server.toml` does not exist |
| `keys/kek` | the KEK, generated on the first start when the configuration names none |
| `store/` | the object store, unless `store.path` names another directory |
| `admin.token` | the first owner's token |
| `admin.password` | the first owner's password, until `admin` sets another |

Back up `keys/kek` outside the host together with the database: without it every secret is lost.
A KEK given by the configuration (`BOSUN_KEYS_KEK`, or `keys.kek_file`) is used instead, and then
none is generated. Several replicas need such a configured KEK and a shared object store; the
data directory of one host is not shared.

Every setting of the configuration can also be given as `BOSUN_<SECTION>_<KEY>`, such as
`BOSUN_DATABASE_MAX_CONNECTIONS`; [design/12-operations.md](../design/12-operations.md) lists them.
