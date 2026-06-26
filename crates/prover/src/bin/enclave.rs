use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use eyre::{Context, ContextCompat, Result};
use zeroize::Zeroizing;
use zone_prover::{
    ProverServer, SignerConfig, SigningKeySource, server::AttestationProviderConfig,
    transport::DEFAULT_VSOCK_PORT,
};

#[derive(Debug, Parser)]
#[command(
    name = "zone-prover-enclave",
    about = "Zone prover server intended to run inside an AWS Nitro enclave"
)]
struct Args {
    /// Vsock port to listen on when --tcp-listen is not set.
    #[arg(long, default_value_t = DEFAULT_VSOCK_PORT)]
    vsock_port: u32,

    /// TCP address for local testing, for example 127.0.0.1:5005.
    #[arg(long)]
    tcp_listen: Option<String>,

    /// File used to load or generate the enclave signing key.
    #[arg(long, default_value = "/data/zone-prover.key")]
    key_file: PathBuf,

    /// Hex private key. Intended only for deterministic local tests.
    #[arg(long, env = "ZONE_PROVER_PRIVATE_KEY_HEX")]
    private_key_hex: Option<String>,

    /// Use an ephemeral key and do not load or write --key-file.
    #[arg(long)]
    ephemeral_key: bool,

    /// Attestation source. Use nsm inside Nitro, none for local tests, file for fixtures.
    #[arg(long, value_enum, default_value_t = AttestationSource::Nsm)]
    attestation_provider: AttestationSource,

    /// Raw COSE_Sign1 Nitro attestation document fixture used with --attestation-provider file.
    #[arg(long, alias = "attestation-doc")]
    attestation_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AttestationSource {
    Nsm,
    File,
    None,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let key_source = if args.ephemeral_key {
        SigningKeySource::Ephemeral
    } else if let Some(hex) = args.private_key_hex {
        SigningKeySource::Hex(Zeroizing::new(hex))
    } else {
        SigningKeySource::File(args.key_file)
    };
    let attestation_provider = match args.attestation_provider {
        AttestationSource::Nsm => AttestationProviderConfig::Nsm,
        AttestationSource::File => AttestationProviderConfig::File(
            args.attestation_file
                .context("--attestation-file is required when --attestation-provider file")?,
        ),
        AttestationSource::None => AttestationProviderConfig::None,
    };

    let server = ProverServer::from_config(SignerConfig {
        key_source,
        attestation_provider,
    })
    .context("failed to initialize prover server")?;

    eprintln!("zone prover signer: {}", server.signer());
    if let Some(addr) = args.tcp_listen {
        eprintln!("listening on tcp://{addr}");
        server.serve_tcp(&addr).await?;
    } else {
        eprintln!("listening on vsock port {}", args.vsock_port);
        server.serve_vsock(args.vsock_port).await?;
    }
    Ok(())
}
