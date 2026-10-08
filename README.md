# lanlink

lanlink lets friends play LAN games like Minecraft over the internet as if they were on the same network. It is a small alternative to Radmin VPN or Hamachi with no account to create and no virtual network adapter to install. One person shares a game running on their computer, and the other gets a local address like `127.0.0.1:25565` on their own computer that leads straight to it.

Connections are end-to-end encrypted, and only people you have allowed can connect to you. Traffic goes directly between the two computers when the network allows it, and through a relay server when it doesn't.

## Features

- No account, no sign-up, no server to run. Your identity is a key pair created on first launch.
- No virtual network adapter or driver. lanlink forwards individual ports, so it needs no admin rights.
- Per-person allowlist. Nobody reaches your games until you click **Allow**.
- TCP and UDP services, so it works for Minecraft Java and for games that use UDP.
- Minecraft "Open to LAN" support. lanlink spots the world you opened, shares it with one click, and makes it show up in your friend's Multiplayer list.
- Direct peer-to-peer connections with NAT hole punching, falling back to a relay. You can run your own relay.
- Live connection info per friend: direct or relayed, latency, jitter and traffic.
- Desktop app with a tray or menu bar icon, plus a command-line version for headless machines.
- Built-in updates: lanlink tells you when a new release is out and can install it for you.

## Platform support

| Platform | Desktop app | Command line | Download |
|---|---|---|---|
| Windows x64 | Yes | Yes | Installer or portable zip |
| macOS, Apple Silicon | Yes | Yes | Disk image (stable and nightly) |
| macOS, Intel | Yes | Yes | Disk image (stable only) |
| Linux | No | Yes, build from source | None |

## Contents

- [How it works](#how-it-works)
- [Install](#install)
- [Using lanlink](#using-lanlink)
- [Settings](#settings)
- [Troubleshooting](#troubleshooting)
- [Security](#security)
- [Command line](#command-line)
- [Self-hosting a relay](#self-hosting-a-relay)
- [Developers](#developers)
- [Contributing](#contributing)
- [License](#license)

## How it works

```
        Host                                         Player
 +-------------------+                       +-------------------+
 | Minecraft         |                       | Minecraft         |
 | (LAN port 51234)  |                       | Direct Connection |
 |        ^          |                       |        |          |
 |        | TCP      |                       |        | TCP      |
 |        |          |    QUIC + TLS 1.3     |        v          |
 |     lanlink <=====+=== direct UDP path ===+==> lanlink        |
 |                   |  (NAT hole punching)  |   127.0.0.1:25565 |
 +---------+---------+                       +---------+---------+
           |                                           |
           |           +---------------------+         |
           +=========> |  relay (fallback)   | <=======+
                       |  forwards encrypted |
                       |  packets only       |
                       +---------------------+
```

### Identity

On first start lanlink generates an Ed25519 key pair and saves the secret key as `identity.key` in the config folder. The public key is your **ID**, the long string you copy from the Peers tab and send to friends. There is no account or central user list. Your ID is the only way to reach you, and it stays the same until you delete `identity.key`.

### Transport

lanlink is built on [iroh](https://github.com/n0-computer/iroh), which connects two computers by ID instead of by IP address. Each connection is a single QUIC connection secured with TLS 1.3, where each side proves it owns the private key for its ID.

iroh first tries to set up a direct UDP path between the two computers, punching through NAT where possible. If that fails, traffic goes through a relay server over HTTPS. iroh keeps trying to upgrade a relayed connection to a direct one, which is why a friend can switch from Relayed to Direct after a few seconds. By default lanlink uses the public relays and address lookup run by n0, the team behind iroh. You can set your own relay in Settings.

### The allowlist handshake

Every connection starts with a handshake on the first QUIC stream. Both sides send a `Hello` with their display name and a `Services` message with the list of services they share.

When a connection arrives from an ID that isn't on your allowlist, lanlink reads only its `Hello` to learn the name, closes the connection, and shows a request with **Allow** and **Ignore** buttons. The unknown peer never sees your services and can't open streams to them. Requests are kept for 10 minutes, at most 20 at a time.

Allowing a peer adds their ID to `allowed_peers` in your config. lanlink then connects to every allowed peer in the background and reconnects with backoff (2 to 30 seconds) when a connection drops or the network changes.

### Control protocol

The first stream of each connection stays open as a control channel. Messages are JSON objects with a `type` field, each prefixed with its length as a 4-byte big-endian integer:

| Message | Purpose |
|---|---|
| `Hello` | Display name, sent first. |
| `Services` | Your shared services: name, protocol, port and options. Sent again whenever the list changes. |
| `Ping`, `Pong` | Sent every second by the side that dialed. The round trip gives the latency and jitter shown in the app. |

The protocol is forward compatible. Unknown fields are ignored, and a message type a build doesn't know is read as `Unknown` and skipped. A newer lanlink can add messages and fields without breaking older ones. The ALPN identifier is `lanlink/1` and only changes for incompatible protocol changes.

### Tunnels: from a shared service to a local address

When you click **Connect** on a friend's service, lanlink opens a listener on `127.0.0.1` on your computer. What happens next depends on the protocol.

**TCP.** Each connection your game makes to the local address becomes a new QUIC stream to the host. The stream starts with a small header naming the service, then carries raw bytes in both directions. The host checks that the service is shared and enabled, connects to it on its own computer (`127.0.0.1` and the service port by default), and copies data both ways. If nothing is listening there, the host resets the stream with an error code and your app shows a "Nothing is listening" error. TCP_NODELAY and small buffers keep latency low.

**UDP.** Packets travel as QUIC datagrams, which are unreliable like UDP itself, so a lost packet never holds up later ones. Each datagram has a 10-byte header with the service index, a flow ID and fragment numbers. Each local program sending to the tunnel gets its own flow, and the host opens a separate local socket per flow, so replies go back to the right program. Packets larger than the path allows are split into up to 255 fragments and put back together on the other side. If one fragment is lost the whole packet is dropped, as with IP fragmentation. Idle flows close after 30 seconds.

### Minecraft Open to LAN

Minecraft announces worlds opened to LAN on the multicast address `224.0.2.60:4445`. lanlink listens there and lists any world it hears under **Detected** in the Hosting tab. A service marked as Minecraft forwards to the port of the detected world when exactly one is open, so it keeps working after Minecraft picks a new port.

On the player's side, lanlink announces each open Minecraft tunnel on the same multicast address over loopback. Minecraft then lists it in the Multiplayer screen as a LAN world pointing at the local tunnel address. Announcements carry a `(lanlink)` marker so lanlink doesn't detect its own announcements as worlds.

### Files

Everything lives in one folder per user:

| OS | Folder |
|---|---|
| Windows | `%APPDATA%\lanlink` |
| macOS | `~/Library/Application Support/lanlink` |
| Linux | `~/.config/lanlink` |

Set the `LANLINK_CONFIG_DIR` environment variable to use a different folder, for example to run a second instance for testing.

| File | Contents |
|---|---|
| `identity.key` | Your 32-byte secret key. Anyone with this file can act as you. |
| `config.json` | Allowed peers and their names, shared services, saved tunnels, relay URL, display name and network options. Shared by the app and the CLI. |
| `app.json` | Desktop app preferences: background mode, launch at login, theme, update check. |
| `lanlink.lock`, `lanlink.pid` | Single-instance lock while lanlink runs. |
| `logs/` | Daily log files (`app.*.log`, `cli.*.log`), 7 kept, plus `latency.*.csv` when latency logging is on. |

Set `RUST_LOG` to change the log level, for example `RUST_LOG=lanlink_core=debug`.

## Install

Downloads are on the [GitHub Releases](https://github.com/joaovitorscr/lanlink/releases) page. There are two channels:

- **Stable** releases are marked **Latest**. Use these unless you want to test new changes.
- **Nightly** releases are marked **Pre-release**. They are built from `main` each day it has new commits, so they are newer but less tested.

lanlink is not code-signed on Windows or notarized on macOS, so both systems warn you the first time you open it. The steps below show how to get past the warning.

### Windows

1. Download `lanlink-<version>-windows-x64-setup.exe`.
2. Run it. By default it installs for your user only, so no admin password is needed, and adds lanlink to the Start menu.
3. If Windows shows "Windows protected your PC" (SmartScreen), click **More info**, then **Run anyway**.
4. If Windows Firewall asks whether to allow lanlink, click **Allow**. Private networks are enough.

If you'd rather not install anything, download `lanlink-<version>-windows-x64.zip` instead, extract it and run `lanlink.exe`.

Both downloads also include `lanlink-cli.exe`, the command-line version. You don't need it to play.

### macOS

1. Download `lanlink-<version>-macos-arm64.dmg` for Apple Silicon (M1 and newer) or `lanlink-<version>-macos-x64.dmg` for Intel Macs. Nightly releases only include the Apple Silicon image.
2. Open the disk image and drag `lanlink` onto the **Applications** shortcut. The `lanlink` file next to it is the command-line version, which you don't need.
3. The first time, right-click `lanlink.app` in Applications and choose **Open**, then click **Open** in the dialog. A plain double-click only says the app can't be checked for malware.

On recent macOS versions the dialog may have no Open button. In that case go to *System Settings > Privacy & Security* and click **Open Anyway**. You can also clear the download quarantine in Terminal:

```sh
xattr -dr com.apple.quarantine /Applications/lanlink.app
```

After the first launch it opens normally.

## Using lanlink

The app has four tabs. **Peers** holds your ID and your friends. **Hosting** lists what you share. **Tunnels** lists what your friends share with you. **Settings** has everything else.

Adding someone works in both directions. Each person adds the other, or allows the other when a request comes in. After that, either of you can share games and the other can join them.

### Hosting a game

1. Open lanlink. In the **Peers** tab, click **Copy** next to your ID and send it to your friend (Discord, WhatsApp, anything).
2. When your friend adds you, a request with their name appears in the Peers tab. Click **Allow**.
3. Start Minecraft and open your world to LAN (Esc > *Open to LAN*).
4. Open the **Hosting** tab. Under **Detected**, click **Share** next to your world.

For other games, or if detection doesn't see your world, click **Add service** and enter a name, the port the game listens on, and TCP or UDP. Tick **Minecraft world or server** for Minecraft so lanlink keeps following the world's port. Leave **Address** empty unless the game only listens on a specific address, such as a LAN IP or `::1`.

The Hosting tab warns "Nothing is listening on this port" when the game isn't running on the shared TCP port. UDP ports can't be checked this way.

### Joining a game

1. Open lanlink and click **Add Peer**. Paste your friend's ID, give them a name if you like, and click **Add**.
2. Wait for your friend to click **Allow**. The Peers tab shows **Connecting**, then **Direct** or **Relayed**. If the connection drops, click **Reconnect**.
3. Open the **Tunnels** tab. Your friend's shared games are listed under their name. Click **Connect**.
4. lanlink opens a local address such as `127.0.0.1:25565` and shows it next to the game. It uses the same port as on your friend's computer when that port is free on yours, and a random free port otherwise.
5. In Minecraft, the world shows up in *Multiplayer* as "<friend> - <game> (lanlink)". You can also use *Direct Connection* with the address from the Tunnels tab.

Connected tunnels are remembered and reopen the next time lanlink starts. Use the toggle next to a tunnel to turn that off, or the **x** to close it.

### Background and tray

Closing the window doesn't quit lanlink by default. It keeps running in the notification area on Windows (the icons next to the clock, check the **^** overflow arrow) or in the menu bar on macOS, so your friends stay connected and your tunnels stay open.

Click the icon or choose **Show lanlink** from its menu to bring the window back. On macOS, clicking lanlink in the Dock also works. The menu shows how many peers are connected. Choose **Quit lanlink** to stop it.

Quitting tells your peers right away that you left, instead of them seeing you as connected for another 30 seconds or so.

Only one copy of lanlink can run at a time per user, because the app and the command-line version share one identity. Starting a second one stops with "lanlink is already running".

## Settings

The **Settings** tab has these sections:

| Section | What you can change |
|---|---|
| Identity | Your display name, which peers see. Your ID, with a Copy button. |
| Network | The relay server (empty means the public relays; restart lanlink after changing it). **Detect Minecraft LAN worlds**. **Log latency**, which writes every ping to a daily CSV in the logs folder and keeps the last 7 days. |
| Background | **Keep running in the background when the window is closed** (on by default). **Launch at login** (off by default). |
| Appearance | Light, dark or system theme. Window transparency. |
| Updates | Turn the update check on or off, **Install updates automatically**, and **Check now**. |
| Files | **Open** buttons for the logs folder and the config folder. |

**Launch at login** starts lanlink hidden in the notification area or menu bar when you log in. On Windows it adds lanlink to your user's startup programs (the per-user Run key). On macOS it adds a LaunchAgent at `~/Library/LaunchAgents/lanlink.plist`. If you move `lanlink.exe` or the app, open lanlink once so the login item follows it.

**Updates.** Stable and nightly builds check GitHub about 10 seconds after starting and then every 6 hours. Stable builds look for a newer stable release, nightly builds for a newer nightly. When one is out, a banner at the top of the window offers **Install update**: lanlink downloads the installer (Windows) or disk image (macOS) for your computer, checks it against the release's `SHA256SUMS`, then quits, installs it and starts again. With **Install updates automatically** on, the download happens in the background and the banner shows **Restart to update**; nothing installs until you click it. The portable Windows zip, and a macOS app run from the disk image instead of Applications, can't update themselves, so the banner links to the release page instead. Builds you compile yourself report the `dev` channel and never check.

### Import and export

**Export config…** saves your peers and their names, shared services, saved tunnels and settings to a JSON file. Your identity key is not included, so importing the file elsewhere does not copy who you are. **Import config…** shows what a file contains, then either **merges** it into your config (the file wins where both have the same peer, service or tunnel) or **replaces** your config with it. The previous config is saved as `config.json.bak` in the config folder first. From a terminal, with lanlink closed: `lanlink config export [file]` and `lanlink config import <file> [--replace]`.

## Troubleshooting

**Relayed instead of Direct.** The Peers tab shows how each friend is connected. Direct is the best case. Relayed means traffic goes through the relay server, which is common when one side is behind CGNAT (many mobile and some home ISPs). It still works but adds latency.

**Connected, but the game says it can't connect.** The host's game isn't listening on the shared port, and the player's lanlink shows "Nothing is listening on ... on <friend>'s computer". Check that the world is open to LAN and that the host's Hosting tab doesn't say "Nothing is listening on this port". Minecraft picks a new port each time you open to LAN. Services marked as Minecraft follow the new port automatically when exactly one LAN world is open. Otherwise, share it again.

**The world doesn't show up under Detected.** Check that **Detect Minecraft LAN worlds** is on in Settings, and that no other program has taken UDP port 4445. Adding the service by hand with **Add service** always works.

**"lanlink is already running (pid N)".** Quit the other copy first. Look for the lanlink icon in the notification area or menu bar. `lanlink id` still works while the app runs.

**Your friend never sees the request.** Check that the whole ID was copied, that both apps are running and show **Online** in the sidebar, and that the firewall prompt was allowed. Requests appear while lanlink is open and expire after 10 minutes.

**Logs.** When asking for help, attach the latest log file from the logs folder. **Open** next to Logs in Settings opens it, or go there directly:

- Windows: `%APPDATA%\lanlink\logs` (paste it into the Explorer address bar)
- macOS: `~/Library/Application Support/lanlink/logs`
- Linux: `~/.config/lanlink/logs`

## Security

What lanlink protects:

- **Encryption.** All traffic between peers is end-to-end encrypted with QUIC and TLS 1.3, including traffic that goes through a relay.
- **Identity.** Peers are identified by Ed25519 public keys and must prove they hold the matching private key, so nobody can pose as a friend without their `identity.key`.
- **Access.** Only IDs on your allowlist can reach your shared services. Unknown peers can send you a connection request with a name, nothing more.
- **Scope.** Only the ports you share are reachable, and only while sharing is on. Nothing else on your computer or network is exposed. Tunnels on the player's side listen on `127.0.0.1` only.
- **Relays.** A relay forwards encrypted packets it can't read. It does see both IDs, both IP addresses, and how much traffic flows and when.

What it doesn't protect:

- **Allowed peers are trusted.** Anyone you allow can connect to every service you share, as if they were on your LAN. Remove peers you no longer play with.
- **The shared game itself.** lanlink doesn't filter what goes through a tunnel. A vulnerable game server is just as vulnerable to an allowed friend.
- **Your secret key.** `identity.key` is stored unencrypted. On macOS and Linux it is created readable only by you.
- **Release binaries are unsigned.** Windows builds aren't code-signed and macOS builds are only ad-hoc signed, not notarized. Download only from this repository's Releases page. Updates are downloaded over HTTPS and only installed if they match the release's `SHA256SUMS`, which guards against corrupted downloads but not against a compromised release.
- **Discovery.** With default settings, n0's public address lookup service sees your ID, and n0's public relays see your ID and IP address. Run your own relay if that matters to you.

## Command line

`lanlink` (`lanlink-cli.exe` in the Windows downloads) uses the same identity and config as the app, so it can't run at the same time as the app. It is mainly useful on headless machines, including Linux.

```sh
lanlink id                                   # print your ID
lanlink allow <id> --name ana                # allow a peer
lanlink requests --secs 30                   # listen for connection requests
lanlink accept <id> --name ana               # allow a peer that asked (same as allow)
lanlink peers                                # list allowed peers
lanlink rename <id> ana                      # change a peer's local name
lanlink remove <id>                          # remove a peer and its saved tunnels
lanlink host --name minecraft --port 25565   # share a local TCP port until Ctrl-C
lanlink host --name game --port 7777 --udp   # share a UDP port
lanlink connect <id> minecraft --local 127.0.0.1:25565   # forward a peer's service
lanlink --version
```

`host` also takes `--host <address>` for services that listen on a LAN address or IPv6. `connect` picks a random local port when `--local` is left out. Run `lanlink <command> --help` for details.

## Self-hosting a relay

When two peers can't punch through NAT, traffic goes through a relay. By default that is one of n0's public relays. `crates/relay` builds `lanlink-relay`, a standalone iroh relay you can run on a small VPS, with Docker Compose and Caddy or with systemd.

See [`deploy/relay/README.md`](deploy/relay/README.md) for ports, setup and health checks. Then set **Relay server** in each user's Settings to your relay's URL, for example `https://relay.example.com`, and restart lanlink.

## Developers

### Workspace

| Crate | Package | What it is |
|---|---|---|
| `crates/core` | `lanlink-core` | iroh endpoint, allowlist, control protocol, service registry, TCP and UDP forwarding, Minecraft LAN discovery, config. No UI dependencies. |
| `crates/cli` | `lanlink-cli` | Headless binary `lanlink`. |
| `crates/app` | `lanlink-app` | [gpui](https://www.gpui.rs) desktop app, binary `lanlink-app` (shipped as `lanlink.exe` and `lanlink.app`). |
| `crates/relay` | `lanlink-relay` | Self-hosted iroh relay for a VPS. |

Inside `crates/core`, `node.rs` runs the endpoint, connections and tunnels, `api.rs` adds the management calls the GUI uses, `protocol.rs` is the wire format, `forward.rs` copies TCP and UDP traffic, and `lan.rs` handles Minecraft LAN announcements.

### Building

You need a stable Rust toolchain.

```sh
cargo build --workspace                  # everything, debug
cargo run -p lanlink-app                 # desktop app
cargo run -p lanlink-cli -- --help       # CLI
cargo run -p lanlink-relay -- --help     # relay
cargo test -p lanlink-core               # tests
```

gpui uses its `runtime_shaders` feature, so macOS builds don't need the Metal toolchain. On Windows, gpui compiles its shaders with `fxc.exe` at build time, so the Windows desktop app can only be built on Windows. The CLI can be cross-compiled. The desktop app doesn't target Linux, but the core, CLI and relay build there.

`crates/app/build.rs` embeds the icon and version info into the Windows executable with `winresource`.

### Scripts

| Script | What it does |
|---|---|
| `scripts/bundle-macos.sh` | Builds the release app into `dist/lanlink.app` (ad-hoc signed) and the CLI into `dist/lanlink`. Options: `--target <triple>` (for example `x86_64-apple-darwin`, after `rustup target add`), `--out <dir>`, `--locked`. The bundle version comes from `LANLINK_VERSION`, else `Cargo.toml`. The release workflow uses it. |
| `scripts/build-windows-cli.sh` | Cross-compiles the CLI to `dist/lanlink-cli.exe` from macOS. Needs `rustup` and `brew install mingw-w64`. |
| `scripts/make-icons.sh` | Regenerates `assets/icon.{png,ico,icns}` and the tray icons from the SVGs in `assets/`. Needs `brew install librsvg` and `python3`. |

### CI

`.github/workflows/build.yml` runs on pushes to `main` and on pull requests:

- Linux: `cargo fmt --check`, core tests, clippy on everything except the desktop app, and a relay build.
- Windows: clippy on the desktop app and CLI, and core tests.
- `cargo deny check advisories` against the RustSec database (settings in `deny.toml`).

CI doesn't run on macOS and doesn't build the Windows app in release mode, to keep it fast. The release workflow builds both. Cargo commands use `--locked`, so commit `Cargo.lock` changes. CI doesn't produce downloads.

### Releases

`.github/workflows/release.yml` publishes two channels:

| Channel | When | Version example | Builds |
|---|---|---|---|
| Nightly | Daily at 05:17 UTC if `main` has new commits since the last nightly, or by hand (Actions > release > Run workflow, channel `nightly`). | `0.0.2-nightly.20261008.12` | Windows installer and zip, macOS Apple Silicon |
| Stable | Push a tag matching the Cargo version, or run the workflow by hand with channel `stable`, which promotes the latest nightly's commit. | `0.0.2` | Windows installer and zip, macOS Apple Silicon and Intel |

Nightlies are GitHub pre-releases and never marked Latest, so `releases/latest` always points at stable. Only the newest 10 nightlies are kept. The release workflow sets `LANLINK_VERSION`, `LANLINK_CHANNEL` and `LANLINK_COMMIT` at build time. The app shows its version and channel at the top of Settings, and `lanlink --version` prints the version.

To cut a stable release:

1. Bump `version` under `[workspace.package]` in `Cargo.toml` and merge it to `main`.
2. Either wait for a nightly to include that commit and run the release workflow by hand with channel `stable`, or tag the commit yourself:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

   The tag must match the Cargo version or the workflow fails.

The update check calls the GitHub releases API for the repository in `build_info::REPO` (`joaovitorscr/lanlink`). Set `LANLINK_REPO=owner/name` at build time to point a fork's builds at its own releases.

## Contributing

Bug reports, ideas and pull requests are welcome. Open an issue to discuss larger changes first. Before sending a pull request, run:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p lanlink-core
```

On macOS or Linux, use `cargo clippy --workspace --exclude lanlink-app --all-targets -- -D warnings` if the app doesn't build on your machine. CI also runs clippy for the app on Windows.

## License

MIT. See [LICENSE](LICENSE).
