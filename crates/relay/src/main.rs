//! `lanlink-relay`: self-hostable iroh relay for lanlink peers that can't hole punch.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{bail, Context, Result};
use clap::Parser;
use iroh_relay::{
    defaults::DEFAULT_RELAY_QUIC_PORT,
    server::{AcmeConfig, CertConfig, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig},
};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "lanlink-relay", version, about = "Self-hosted iroh relay for lanlink")]
struct Cli {
    /// Plain HTTP listener. Serves the relay itself when no TLS is configured
    /// (e.g. behind a reverse proxy); otherwise only the captive-portal probe.
    #[arg(long, default_value = "0.0.0.0:3340")]
    http_addr: SocketAddr,

    /// HTTPS listener, used when TLS is configured.
    #[arg(long, default_value = "0.0.0.0:443")]
    https_addr: SocketAddr,

    /// PEM certificate chain for built-in TLS.
    #[arg(long, requires = "tls_key", conflicts_with = "acme_domain")]
    tls_cert: Option<PathBuf>,

    /// PEM private key for built-in TLS.
    #[arg(long, requires = "tls_cert")]
    tls_key: Option<PathBuf>,

    /// Obtain a Let's Encrypt certificate for this domain (needs --http-addr on port 80
    /// or --https-addr on 443 reachable from the internet).
    #[arg(long, requires = "acme_contact")]
    acme_domain: Option<String>,

    /// Contact email for Let's Encrypt.
    #[arg(long)]
    acme_contact: Option<String>,

    /// Directory to cache ACME certificates in.
    #[arg(long, default_value = "/var/lib/lanlink-relay/acme")]
    acme_cache: PathBuf,

    /// Use the Let's Encrypt staging directory (for testing).
    #[arg(long)]
    acme_staging: bool,

    /// Also run QUIC address discovery (iroh's STUN replacement). Requires TLS.
    #[arg(long)]
    stun: bool,

    /// UDP address for QUIC address discovery.
    #[arg(long, default_value_t = SocketAddr::from(([0, 0, 0, 0], DEFAULT_RELAY_QUIC_PORT)))]
    stun_addr: SocketAddr,

    /// Optional Prometheus metrics listener.
    #[arg(long)]
    metrics_addr: Option<SocketAddr>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    let cli = Cli::parse();
    let config = build_config(&cli)?;
    let mut server = Server::spawn(config).await.context("spawn relay server")?;

    if let Some(addr) = server.http_addr() {
        info!(%addr, "HTTP listening");
    }
    if let Some(addr) = server.https_addr() {
        info!(%addr, "HTTPS listening");
    }
    if let Some(addr) = server.quic_addr() {
        info!(%addr, "QUIC address discovery listening");
    }

    tokio::select! {
        biased;
        _ = tokio::signal::ctrl_c() => info!("shutting down"),
        res = server.join() => info!(?res, "server task exited"),
    }
    server.shutdown().await.context("shutdown")?;
    Ok(())
}

fn build_config(cli: &Cli) -> Result<ServerConfig> {
    let mut relay = RelayConfig::new(cli.http_addr);
    relay.tls = tls_config(cli)?.map(|cert| TlsConfig::new(cli.https_addr, cert));

    if cli.stun && relay.tls.is_none() {
        bail!("--stun needs TLS: pass --tls-cert/--tls-key or --acme-domain");
    }

    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.quic = cli.stun.then(|| QuicConfig::new(cli.stun_addr));
    config.metrics_addr = cli.metrics_addr;
    Ok(config)
}

fn tls_config(cli: &Cli) -> Result<Option<CertConfig>> {
    let builder = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("rustls protocol versions")?
    .with_no_client_auth();

    if let (Some(cert), Some(key)) = (&cli.tls_cert, &cli.tls_key) {
        let certs = CertificateDer::pem_file_iter(cert)
            .with_context(|| format!("read {}", cert.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("parse {}", cert.display()))?;
        let key = PrivateKeyDer::from_pem_file(key)
            .with_context(|| format!("read key {}", key.display()))?;
        let server_config = builder.with_single_cert(certs, key).context("tls config")?;
        return Ok(Some(CertConfig::Manual { server_config }));
    }

    if let Some(domain) = &cli.acme_domain {
        let contact = cli.acme_contact.as_deref().unwrap_or_default();
        let acme_config = AcmeConfig::letsencrypt(!cli.acme_staging)
            .domains(vec![domain.clone()])
            .contact(vec![format!("mailto:{contact}")])
            .cache_path(cli.acme_cache.clone());
        return Ok(Some(CertConfig::LetsEncrypt {
            acme_config,
            server_config_builder: builder,
        }));
    }

    Ok(None)
}
