# Bosun as a quadlet under rootless podman

`deploy/bosun.container` is a quadlet: podman turns it into the systemd service `bosun.service`.
It runs the image under rootless podman with a named volume `bosun-data`, publishes port 8080 and
takes the database address from the podman secret `bosun-database-url`, so the address appears on
no command line. Postgres runs elsewhere.

As the user that runs Bosun:

```sh
printf '%s' 'postgres://bosun:change-me@db.example.org:5432/bosun' |
  podman secret create bosun-database-url -
mkdir -p ~/.config/containers/systemd
cp deploy/bosun.container ~/.config/containers/systemd/
sudo loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user start bosun
podman exec bosun cat /bosun-data/admin.token
```

Lingering keeps the service running without a session of the user and starts it at boot.
`journalctl --user -u bosun` shows the server's log. Pin a timestamp tag in `Image=` for
production; an upgrade is changing it, `systemctl --user daemon-reload` and
`systemctl --user restart bosun`.

For a rootful service, put the file into `/etc/containers/systemd/` and use `systemctl` without
`--user`.
