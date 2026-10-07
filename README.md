# lanlink

Lightweight peer-to-peer port forwarding for playing games with friends. Radmin VPN alternative.

- `crates/core`  — iroh endpoint, allowlist, service registry, TCP/UDP forwarding. No UI deps.
- `crates/cli`   — `lanlink` headless binary (host / connect / peers).
- `crates/app`   — gpui desktop app.
- `crates/relay` — self-hosted iroh relay for the VPS.

Security: Ed25519 identities, QUIC + TLS 1.3, allowlist enforced at handshake.
Latency: direct UDP path when hole punching works, relay fallback otherwise.
