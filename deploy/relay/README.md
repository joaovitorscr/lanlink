# Deploying lanlink-relay

When two peers can't hole punch (for example, one is behind CGNAT), their traffic
goes through a relay. These files run your own relay on a small VPS so that traffic
doesn't go through the public iroh relays.

## Ports

| Port     | Proto | Purpose                                                         |
|----------|-------|-----------------------------------------------------------------|
| 443      | tcp   | Relay over HTTPS/WebSocket (Caddy or built-in TLS)              |
| 80       | tcp   | ACME HTTP challenge and captive-portal probe (`/ping`)          |
| 7842     | udp   | QUIC address discovery, only with `--stun` and built-in TLS     |

iroh 1.x doesn't use classic STUN on 3478/udp. It runs QUIC address discovery
(QAD) on 7842/udp instead. QAD needs the relay's own TLS certificate, so it only
works with `--tls-cert/--tls-key` or `--acme-domain`, not behind Caddy. Without
QAD, clients still find their public address through the relay connection.

Port 3340/tcp is the relay's plain HTTP listener. Keep it private (bound to
localhost or the Docker network) when it sits behind a proxy.

## Option A: Docker Compose + Caddy

```sh
# set RELAY_DOMAIN in docker-compose.yml, point the domain's DNS at the VPS
docker compose -f deploy/relay/docker-compose.yml up -d --build
```

## Option B: systemd

```sh
cargo build --release -p lanlink-relay
sudo install target/release/lanlink-relay /usr/local/bin/
sudo cp deploy/relay/lanlink-relay.service /etc/systemd/system/
sudo systemctl enable --now lanlink-relay
```

Then put Caddy in front using `Caddyfile` (change the upstream to
`127.0.0.1:3340`), or edit `ExecStart` to use built-in TLS:

```sh
lanlink-relay --http-addr 0.0.0.0:80 --https-addr 0.0.0.0:443 \
  --acme-domain relay.example.com --acme-contact you@example.com --stun
```

## Check it

```sh
curl https://relay.example.com/healthz
curl https://relay.example.com/ping
```

## Clients

Each user sets the relay in the app: open the **Settings** tab, enter
`https://relay.example.com` in **Relay server**, click **Save**, then restart lanlink.

Without the app (e.g. on a headless machine running `lanlink-cli`), set `relay_url`
in `config.json` in the lanlink config folder (`%APPDATA%\lanlink` on Windows,
`~/Library/Application Support/lanlink` on macOS, `~/.config/lanlink` on Linux):

```json
{
  "relay_url": "https://relay.example.com"
}
```

Keep the other keys in the file as they are.
