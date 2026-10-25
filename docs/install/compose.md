# Bosun and Postgres with compose

`deploy/compose.yaml` runs Bosun and a Postgres 17 next to it, each with a named volume. It needs
one variable, the password of the database, which takes letters and digits only because it goes
into the database address:

```sh
export BOSUN_DATABASE_PASSWORD="$(openssl rand -hex 16)"
docker compose -f deploy/compose.yaml up --detach --wait
docker compose -f deploy/compose.yaml exec bosun cat /bosun-data/admin.token
```

`--wait` returns once Postgres answers and `bosun-server check` passes. `podman compose` takes the
same file. Keep the password: the database keeps the one of its first start.

| Variable | Default | Meaning |
|----------|---------|---------|
| `BOSUN_DATABASE_PASSWORD` | none, required | the password of the user `bosun` of the database |
| `BOSUN_IMAGE` | `docker.io/globalartltd/bosun:latest` | the image; pin a timestamp tag in production |
| `BOSUN_PUBLISH` | `8080` | where the host publishes port 8080, such as `127.0.0.1:8080` behind a proxy on the same host |

An upgrade is `docker compose -f deploy/compose.yaml pull` and `up --detach --wait` again. The
volumes `data` and `postgres` keep the KEK, the stored objects, the owner and the database; back
up both.
