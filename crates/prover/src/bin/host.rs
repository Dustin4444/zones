use std::{fs, path::PathBuf};

use alloy_primitives::{Address, B256};
use clap::{Parser, Subcommand};
use eyre::{Context, ContextCompat, Result, bail};
use tokio::io::{AsyncRead, AsyncWrite};
use zone_prover::{
    NitroPcr, NitroVerifierConfig, ProofRequest, RegistrationReport, encode_host_proof,
    parse_b256_hex, parse_nitro_pcr_hex,
    server::{EnclaveRequest, EnclaveResponse},
    transport::{
        DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_VSOCK_PORT, connect_tcp, connect_vsock,
        read_json_frame, write_json_frame,
    },
    types::BatchWitness,
    verify_registration_report,
};

trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

#[derive(Debug, Parser)]
#[command(
    name = "zone-prover-host",
    about = "Host-side client for the Nitro Zone prover enclave"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send a BatchWitness to the enclave and write verifierConfig/proof output.
    Prove {
        /// Enclave CID. Ignored when --tcp is set.
        #[arg(long)]
        cid: Option<u32>,

        /// Enclave vsock port.
        #[arg(long, default_value_t = DEFAULT_VSOCK_PORT)]
        port: u32,

        /// TCP address for local testing, for example 127.0.0.1:5005.
        #[arg(long)]
        tcp: Option<String>,

        /// JSON BatchWitness file.
        #[arg(long)]
        witness: PathBuf,

        /// JSON NitroVerifierConfig file with the expected signer/PCR pins.
        #[arg(long)]
        config: PathBuf,

        /// Optional 32-byte request id hex. Generated randomly when omitted.
        #[arg(long, value_parser = parse_b256_arg)]
        request_id: Option<B256>,

        /// Optional 32-byte nonce hex. Generated randomly when omitted.
        #[arg(long, value_parser = parse_b256_arg)]
        nonce: Option<B256>,

        /// JSON output path containing verifierConfig, proof, and BatchOutput.
        #[arg(long)]
        out: PathBuf,
    },

    /// Ask the enclave for its signer and registration material.
    Register {
        /// Enclave CID. Ignored when --tcp is set.
        #[arg(long)]
        cid: Option<u32>,

        /// Enclave vsock port.
        #[arg(long, default_value_t = DEFAULT_VSOCK_PORT)]
        port: u32,

        /// TCP address for local testing, for example 127.0.0.1:5005.
        #[arg(long)]
        tcp: Option<String>,

        /// Optional 32-byte request id hex. Generated randomly when omitted.
        #[arg(long, value_parser = parse_b256_arg)]
        request_id: Option<B256>,

        /// Optional 32-byte nonce hex. Generated randomly when omitted.
        #[arg(long, value_parser = parse_b256_arg)]
        nonce: Option<B256>,

        /// Expected enclave signer address.
        #[arg(long)]
        expected_signer: Address,

        /// Expected Nitro PCR0 SHA-384 measurement.
        #[arg(long, value_parser = parse_nitro_pcr_arg)]
        expected_pcr0: NitroPcr,

        /// Expected Nitro PCR1 SHA-384 measurement.
        #[arg(long, value_parser = parse_nitro_pcr_arg)]
        expected_pcr1: NitroPcr,

        /// Expected Nitro PCR2 SHA-384 measurement.
        #[arg(long, value_parser = parse_nitro_pcr_arg)]
        expected_pcr2: NitroPcr,

        /// JSON output path for the registration report.
        #[arg(long)]
        out: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Prove {
            cid,
            port,
            tcp,
            witness,
            config,
            request_id,
            nonce,
            out,
        } => {
            let witness: BatchWitness = read_json_file(&witness)?;
            let verifier_config: NitroVerifierConfig = read_json_file(&config)?;
            verifier_config
                .validate()
                .context("invalid Nitro verifier config")?;
            let request = ProofRequest {
                version: zone_prover::protocol::PROTOCOL_VERSION,
                request_id: request_id_or_random(request_id)?,
                nonce: nonce_or_random(nonce)?,
                verifier_config,
                witness,
            };

            let response = round_trip(
                cid,
                port,
                tcp.as_deref(),
                EnclaveRequest::Prove(Box::new(request.clone())),
            )
            .await?;
            let EnclaveResponse::Proof(envelope) = response else {
                return handle_non_proof_response(response);
            };
            let host_proof = encode_host_proof(&request, &envelope)
                .context("enclave proof failed host-side verification")?;
            write_json_file(&out, &host_proof)?;
            eprintln!("wrote host proof for signer {}", envelope.signer);
        }
        Command::Register {
            cid,
            port,
            tcp,
            request_id,
            nonce,
            expected_signer,
            expected_pcr0,
            expected_pcr1,
            expected_pcr2,
            out,
        } => {
            let response = round_trip(
                cid,
                port,
                tcp.as_deref(),
                EnclaveRequest::Register {
                    version: zone_prover::protocol::PROTOCOL_VERSION,
                    request_id: request_id_or_random(request_id)?,
                    nonce: nonce_or_random(nonce)?,
                },
            )
            .await?;
            let EnclaveResponse::Registration(report) = response else {
                return handle_non_registration_response(response);
            };
            verify_registration_report(&report)
                .context("registration report failed verification")?;
            verify_registration_pins(
                &report,
                expected_signer,
                &expected_pcr0,
                &expected_pcr1,
                &expected_pcr2,
            )?;
            write_json_file(&out, &*report)?;
            eprintln!("wrote registration report for signer {}", report.signer);
        }
    }
    Ok(())
}

async fn round_trip(
    cid: Option<u32>,
    port: u32,
    tcp: Option<&str>,
    request: EnclaveRequest,
) -> Result<EnclaveResponse> {
    let mut stream = connect(cid, port, tcp).await?;
    write_json_frame(&mut stream, &request)
        .await
        .context("failed to write request frame")?;
    read_json_frame(&mut stream, DEFAULT_MAX_RESPONSE_BYTES)
        .await
        .context("failed to read response frame")
}

async fn connect(
    cid: Option<u32>,
    port: u32,
    tcp: Option<&str>,
) -> Result<Box<dyn AsyncReadWrite>> {
    if let Some(addr) = tcp {
        return Ok(Box::new(connect_tcp(addr).await?));
    }
    let cid = cid.context("--cid is required unless --tcp is set")?;
    Ok(Box::new(connect_vsock(cid, port).await?))
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

fn write_json_file<T: serde::Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

fn parse_b256_arg(input: &str) -> Result<B256, String> {
    parse_b256_hex(input).map_err(|err| err.to_string())
}

fn parse_nitro_pcr_arg(input: &str) -> Result<NitroPcr, String> {
    parse_nitro_pcr_hex(input).map_err(|err| err.to_string())
}

fn request_id_or_random(request_id: Option<B256>) -> Result<B256> {
    match request_id {
        Some(request_id) => Ok(request_id),
        None => random_challenge_b256("request id"),
    }
}

fn nonce_or_random(nonce: Option<B256>) -> Result<B256> {
    match nonce {
        Some(nonce) => Ok(nonce),
        None => random_challenge_b256("nonce"),
    }
}

fn random_challenge_b256(label: &'static str) -> Result<B256> {
    B256::try_random().map_err(|err| eyre::eyre!("failed to generate random {label}: {err}"))
}

fn verify_registration_pins(
    report: &RegistrationReport,
    expected_signer: Address,
    expected_pcr0: &NitroPcr,
    expected_pcr1: &NitroPcr,
    expected_pcr2: &NitroPcr,
) -> Result<()> {
    if report.signer != expected_signer {
        bail!(
            "registration signer mismatch: expected {expected_signer}, got {}",
            report.signer
        );
    }
    compare_pcr("PCR0", expected_pcr0, &report.expected_pcr0)?;
    compare_pcr("PCR1", expected_pcr1, &report.expected_pcr1)?;
    compare_pcr("PCR2", expected_pcr2, &report.expected_pcr2)?;
    Ok(())
}

fn compare_pcr(label: &'static str, expected: &NitroPcr, actual: &NitroPcr) -> Result<()> {
    if actual != expected {
        bail!("registration {label} mismatch");
    }
    Ok(())
}

fn handle_non_proof_response<T>(response: EnclaveResponse) -> Result<T> {
    match response {
        EnclaveResponse::Error { message } => bail!("enclave returned error: {message}"),
        EnclaveResponse::Registration(_) => bail!("enclave returned registration, expected proof"),
        EnclaveResponse::Proof(_) => unreachable!(),
    }
}

fn handle_non_registration_response<T>(response: EnclaveResponse) -> Result<T> {
    match response {
        EnclaveResponse::Error { message } => bail!("enclave returned error: {message}"),
        EnclaveResponse::Proof(_) => bail!("enclave returned proof, expected registration"),
        EnclaveResponse::Registration(_) => unreachable!(),
    }
}
