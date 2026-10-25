# A reverse proxy in front of Bosun

Bosun serves the API, the install script and the agent stream on one port, without TLS. The
proxy in front of it terminates TLS and:

- speaks HTTP/2 to Bosun on the paths of the agent stream, `/bosun.agent.v1.`, which is gRPC;
- sets no request timeout on those paths, and an idle timeout above the agents' keepalive interval
  of 30 seconds;
- passes the `Host` of the request and sets `X-Forwarded-Proto`, because the install script that
  Bosun serves names the address it was fetched from, and the agents then connect there;
- does not buffer responses, so that followed logs arrive as they are written.

## nginx

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name ci.example.org;

    ssl_certificate     /etc/ssl/ci.example.org/fullchain.pem;
    ssl_certificate_key /etc/ssl/ci.example.org/privkey.pem;

    client_max_body_size 64m;

    location /bosun.agent.v1. {
        grpc_pass grpc://127.0.0.1:8080;
        grpc_read_timeout 1d;
        grpc_send_timeout 1d;
        client_body_timeout 1d;
    }

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_buffering off;
        proxy_read_timeout 1h;
    }
}
```

## Caddy

Caddy obtains the certificate, keeps the `Host` and sets `X-Forwarded-Proto` by itself:

```caddyfile
ci.example.org {
    reverse_proxy /bosun.agent.v1.* h2c://127.0.0.1:8080

    reverse_proxy 127.0.0.1:8080 {
        flush_interval -1
    }
}
```

## Checking it

From a host where agents will run:

```sh
curl -fsSL https://ci.example.org/install-agent.sh | grep "^server="
```

prints `server='https://ci.example.org'` when the proxy passes the host and the scheme.
