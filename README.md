# lanlink

lanlink lets friends play LAN games (like Minecraft) together over the internet, as if they were on the same network. It's a small alternative to Radmin VPN: no account and no virtual network adapter. One person hosts a game and shares it, and the other person joins through a local address on their own computer.

Connections are end-to-end encrypted. Only people you've allowed can connect. When possible, traffic goes straight between the two computers. When that fails, it goes through a relay.

## For the host (Windows)

1. Go to the project's **GitHub Releases** page and download `lanlink-<version>-windows-x64-setup.exe` from the release marked **Latest**. Releases marked **Pre-release** are nightly builds: newer, but less tested.
2. Run the installer. It installs for your user only (no admin password) and puts lanlink in the Start menu. If you'd rather not install anything, the `lanlink-<version>-windows-x64.zip` next to it is a portable version: extract it and run `lanlink.exe`.
3. Open lanlink.
   - If Windows shows "Windows protected your PC" (SmartScreen), click **More info**, then **Run anyway**. The app isn't code-signed, so Windows shows this warning.
   - If Windows Firewall asks whether to allow lanlink, click **Allow** (private networks are enough).
4. In the **Peers** tab, copy your ID and send it to your friend (Discord, WhatsApp, anything).
5. When your friend connects, a banner appears asking whether to let them in. Click **Allow**.
6. Start Minecraft and open your world to LAN (Esc → *Open to LAN*).
7. Open the **Hosting** tab. Either:
   - click **Share** next to the Minecraft world that lanlink detected, or
   - add Minecraft as a service by hand, with the port Minecraft printed in chat.

Keep lanlink running while you play. Closing the window doesn't quit it: lanlink stays in the notification area (the icons next to the clock; check the **^** overflow arrow), and your friend stays connected. Click the lanlink icon to bring the window back, or right-click it and choose **Quit lanlink** to stop. You can change this in Settings.

`lanlink-cli.exe` next to it is a command-line version. You don't need it, and it can't run while the app is open: both use the same identity, so the second one stops with "lanlink is already running".

## For the player

1. Get lanlink: the Windows installer described above, or the macOS disk image (see [macOS](#macos) below).
2. Open lanlink. In the **Peers** tab, add your friend using the ID they sent you.
3. Wait for your friend to click **Allow**. If the connection drops, click **Reconnect**.
4. Open the **Tunnels** tab. It shows a local address such as `127.0.0.1:25565` for each game your friend shares.
5. In Minecraft, go to *Multiplayer → Direct Connection* and enter that address.

The tunnel only works while lanlink is running. Closing the window keeps it running in the notification area on Windows or the menu bar on macOS. Use **Quit lanlink** from that icon's menu when you're done.

## macOS

1. From the release marked **Latest** on GitHub Releases, download `lanlink-<version>-macos-arm64.dmg` for Apple Silicon (M1 and newer) or `lanlink-<version>-macos-x64.dmg` for Intel Macs. Nightly builds only ship the Apple Silicon image.
2. Open the disk image and drag `lanlink` onto the **Applications** shortcut. The `lanlink` file next to it is the command-line version, which you don't need.
3. The first time, **right-click `lanlink.app` and choose Open**, then click **Open** in the dialog. The app isn't notarized by Apple, so a plain double-click only says it can't be checked for malware. On recent macOS versions, if there's no Open button, go to *System Settings → Privacy & Security* and click **Open Anyway**. Alternatively, clear the download quarantine in Terminal:

   ```sh
   xattr -dr com.apple.quarantine /Applications/lanlink.app
   ```

   After the first launch it opens normally.

## Settings

The **Settings** tab lets you change your display name and the relay server, and has a button that opens the logs folder. Under **Background**:

- **Keep running in the background when the window is closed** (on by default). Closing the window hides it and lanlink keeps running in the notification area (Windows) or menu bar (macOS). The icon's menu shows how many peers are connected, and has **Show lanlink** and **Quit lanlink**. On macOS, clicking lanlink in the Dock also brings the window back. Turn this off to make closing the window quit lanlink.
- **Launch at login** (off by default). Starts lanlink hidden in the notification area or menu bar when you log in. On Windows this adds lanlink to your user's startup programs; on macOS it adds a LaunchAgent (`~/Library/LaunchAgents/lanlink.plist`). If you move `lanlink.exe` or the app, open lanlink once so the login item follows it.

Quitting tells your peers right away that you left, instead of them seeing you as connected for another ~30 seconds.

Under **Updates**, lanlink checks GitHub for a newer release shortly after it starts and then every 6 hours. Stable builds look for the next stable release, nightly builds for the next nightly. When one is out, a banner at the top of the window links to its download page. You can turn the check off there, or click **Check now**. lanlink never downloads or installs anything by itself.

## Troubleshooting

- **Relayed vs Direct.** The Peers tab shows how you're connected. *Direct* is the best case. *Relayed* means traffic goes through the relay server (common when one side is behind CGNAT, as with many mobile and some home ISPs). Relayed still works but adds some latency.
- **Can connect but the game says nothing is listening.** The host's game isn't open on the shared port. Check that the world is opened to LAN and that the port in the Hosting tab matches the one Minecraft shows. Minecraft picks a new port each time you open to LAN, so share it again after restarting.
- **"lanlink is already running (pid N)".** Only one lanlink can run at a time per user, because the app and `lanlink-cli` share one identity. Quit the other copy first (check the notification area or menu bar for the lanlink icon). `lanlink id` still works while the app runs.
- **Friend never sees the request.** Double-check that the whole ID was copied, that both apps are running, and that the firewall prompt was allowed.
- **Logs.** When asking for help, send the latest log file:
  - Windows: `%APPDATA%\lanlink\logs` (paste into the Explorer address bar)
  - macOS: `~/Library/Application Support/lanlink/logs`

  The **Open logs** button in Settings opens this folder too.

## Developers

### Workspace

| Crate          | What it is                                                         |
|----------------|--------------------------------------------------------------------|
| `crates/core`  | iroh endpoint, allowlist, service registry, TCP/UDP forwarding. No UI deps. |
| `crates/cli`   | `lanlink` headless binary.                                          |
| `crates/app`   | gpui desktop app (`lanlink-app`).                                   |
| `crates/relay` | Self-hosted iroh relay for a VPS.                                   |

Security: Ed25519 identities, QUIC + TLS 1.3, and an allowlist enforced at the handshake.

### Building

```sh
cargo build --workspace                   # everything, debug
cargo run -p lanlink-app                  # GUI
cargo run -p lanlink-cli -- --help        # CLI
cargo test -p lanlink-core
```

gpui uses the `runtime_shaders` feature, so macOS builds don't need the Metal toolchain. On Windows, gpui compiles its shaders with `fxc.exe` at build time, so **the GUI can only be built on Windows** (CI does this). The CLI can be cross-compiled.

### Scripts

- `scripts/bundle-macos.sh` builds the release GUI into `dist/lanlink.app` (ad-hoc signed, opens with a double-click) and the CLI into `dist/lanlink`. Options: `--target <triple>` (e.g. `x86_64-apple-darwin`, needs `rustup target add`), `--out <dir>`, `--locked`. The bundle version comes from `LANLINK_VERSION`, else `Cargo.toml`. The release workflow uses this script.
- `scripts/build-windows-cli.sh` cross-compiles the CLI to `dist/lanlink-cli.exe` from macOS. Needs `rustup` and `brew install mingw-w64`.
- `scripts/make-icons.sh` regenerates `assets/icon.{png,ico,icns}` from `assets/icon.svg`. Needs `brew install librsvg`.

`crates/app/build.rs` embeds the icon and version info into the Windows executable (via `winresource`).

### CI and releases

**CI** (`.github/workflows/build.yml`) runs on pushes to `main` and on pull requests. It checks formatting, runs the core tests on Linux and Windows, runs clippy on Linux, runs clippy for the GUI and CLI on Windows, and checks dependencies against the RustSec advisory database with `cargo deny` (settings in `deny.toml`). Cargo commands use `--locked`, so commit `Cargo.lock` changes. CI does not produce downloads.

**Releases** (`.github/workflows/release.yml`) have two channels:

| Channel | How it's published | Version | Platforms |
|---|---|---|---|
| Nightly | Daily at 05:17 UTC, only if `main` has new commits. Or by hand: Actions, release, Run workflow, channel nightly. | `0.1.0-nightly.20261008.12` | Windows |
| Stable | Push a tag matching the Cargo version, e.g. `git tag v0.2.0 && git push origin v0.2.0`. Or by hand with channel stable, which promotes the latest nightly's commit. | `0.2.0` | Windows, macOS (Apple Silicon and Intel) |

Nightlies are GitHub pre-releases and never marked Latest, so `releases/latest` always points at stable. Only the newest 10 nightlies are kept. The app shows its version and channel at the top of Settings, and `lanlink-cli --version` prints the version.

The app's update check calls the GitHub releases API for the repo in `build_info::REPO` (`joaovitorscr/lanlink`; set `LANLINK_REPO=owner/name` at build time for a fork). Local builds have channel `dev` and never check. The API needs no token, but only for a public repo: while this repo is private the check gets a 404, which is logged at debug level and shown in Settings as "Couldn't check for updates".

To cut a stable release, bump `version` under `[workspace.package]` in `Cargo.toml`, merge it, wait for a nightly to include it, then run the workflow with channel stable.

The repo is private, so Actions minutes count against the account's quota, with Windows billed at 2x and macOS at 10x. That is why nightlies are Windows only and CI skips macOS.

### Relay

See [`deploy/relay`](deploy/relay/README.md) for running your own relay on a VPS.
