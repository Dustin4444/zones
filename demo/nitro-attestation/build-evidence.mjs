#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const baseDir = dirname(fileURLToPath(import.meta.url));
const artifactDir = join(baseDir, "artifacts");

const readJson = (name) => JSON.parse(readFileSync(join(artifactDir, name), "utf8"));
const readText = (name) => readFileSync(join(artifactDir, name), "utf8").trim();
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const hexBytes = (value) => Buffer.from(value.replace(/^0x/, ""), "hex");
const with0x = (value) => `0x${value.replace(/^0x/, "")}`;
const hexQuantity = (value) => Number.parseInt(value, 16);

const registration = readJson("registration.json");
const receipt = readJson("receipt.json");
const transactionResponse = readJson("transaction.json");
const transaction = transactionResponse.result;
const block = readJson("block.json").result;
const transactionTrace = readJson("transaction-trace.json");
const ethCallRequest = readJson("eth-call-request.json");
const ethCallResponse = readJson("eth-call-response.json");
const decodedOutput = readJson("decoded-precompile-output.json");
const decoded = decodedOutput[0];
const devnetRpc = readJson("devnet-rpc.json");
const devnetPods = readJson("devnet-pods.json");
const tempoPr = readJson("tempo-pr.json");
const tempoImage = readJson("tempo-image-inspect.json");
const imageBuild = readJson("github-image-build.json");
const workflowSummary = readJson("workflow-summary.json");
const eifBuild = readJson("eif-build.json");
const enclave = readJson("enclaves-current.json")[0];
const awsInstance = readJson("aws-instance.json");
const dockerImage = readJson("docker-image-inspect.json")[0];
const bindings = readJson("registration-bindings.json");

const attestationBytes = hexBytes(registration.attestationDoc);
const inputBytes = hexBytes(transaction.input);
const resultBytes = hexBytes(ethCallResponse.result);
const attestationTimestamp = Number(decoded[1]);
const successfulWorkflow = workflowSummary.successfulRetry;
const workflowDurationSeconds =
  (Date.parse(successfulWorkflow.finishedAt) - Date.parse(successfulWorkflow.startedAt)) / 1000;
const eifSha384 = readText("zone-prover.eif.sha384").split(/\s+/, 1)[0];
const eifSize = Number(readText("zone-prover.eif.size").match(/\b(\d+) bytes$/)?.[1]);

if (!Number.isSafeInteger(attestationTimestamp) || !Number.isSafeInteger(eifSize)) {
  throw new Error("captured attestation timestamp or EIF size is invalid");
}

const artifactMetadata = {
  "aws-ami.json": ["AWS AMI", "aws", "Amazon Linux AMI metadata used by the enclave parent instance."],
  "aws-instance-status.json": ["EC2 status checks", "aws", "Captured EC2 system and instance reachability checks."],
  "aws-instance-type.json": ["EC2 instance type", "aws", "m6i.xlarge Nitro Enclaves capability metadata."],
  "aws-instance.json": ["EC2 instance", "aws", "Redacted instance, Nitro Enclaves, IMDSv2, placement, and deployment tag metadata."],
  "block.json": ["Verification block", "chain", "Raw JSON-RPC block response for the mined verification transaction."],
  "decoded-precompile-output.json": ["Decoded verifier output", "chain", "ABI-decoded TIP-1090 return value from the same-block call."],
  "devnet-rpc.json": ["Devnet RPC snapshot", "chain", "Client version, chain ID, head block, and active fork schedule."],
  "devnet-pods.json": ["Devnet pod image IDs", "chain", "Running node and validator pods with the immutable OCI digest actually pulled by Kubernetes."],
  "docker-image-inspect.json": ["Prover image inspect", "nitro", "Docker image ID and the exact enclave command embedded in Config.Cmd."],
  "dockerfile.sha256": ["Dockerfile checksum", "checksum", "SHA-256 checksum of the Dockerfile used to build the prover image."],
  "eif-build.json": ["EIF build measurements", "nitro", "PCR0, PCR1, and PCR2 emitted while building the enclave image file."],
  "evidence-verifier.txt": ["Verification report", "verification", "Output of the independent, fail-closed evidence bundle verifier."],
  "enclave-run.json": ["Enclave launch result", "nitro", "Nitro CLI response for launching the non-debug enclave."],
  "enclaves-current.json": ["Running enclave", "nitro", "Current non-debug enclave state, resources, flags, and PCR measurements."],
  "enclaves.json": ["Enclave capture", "nitro", "Nitro CLI enclave description captured during verification."],
  "eth-call-request.json": ["Same-block eth_call request", "chain", "Exact JSON-RPC call data and block tag used for deterministic replay."],
  "eth-call-response.json": ["Same-block eth_call response", "chain", "Raw ABI return bytes produced by the TIP-1090 precompile."],
  "genesis.json": ["Devnet genesis", "chain", "Complete devnet genesis showing the T9 fork active from timestamp zero."],
  "github-image-build.json": ["Tempo image build", "provenance", "Successful GitHub Actions image build at the PR commit."],
  "receipt.json": ["Transaction receipt", "chain", "Successful onchain verification receipt including fee log and gas usage."],
  "registration-bindings.json": ["Registration bindings", "nitro", "Independent public-key, nonce, and challenge comparisons."],
  "registration.json": ["Registration report", "nitro", "AWS-signed attestation document and the verified Zones registration report."],
  "registration.json.sha256": ["Registration checksum", "checksum", "Host-captured SHA-256 checksum for the registration report."],
  "registration.stderr": ["Host verifier output", "nitro", "Host confirmation that the attested signer registration was accepted."],
  "registration.stdout": ["Host verifier stdout", "nitro", "Captured standard output from the host registration verifier."],
  "ssm-instance.json": ["SSM managed instance", "aws", "Online SSM agent and Amazon Linux platform evidence."],
  "tempo-image-inspect.json": ["Tempo image manifest", "provenance", "Multi-platform image digest, per-platform manifests, and embedded source revision."],
  "tempo-image-manifest.json": ["Tempo OCI index", "provenance", "Raw OCI image index for the exact devnet image tag."],
  "tempo-pr.json": ["Tempo PR", "provenance", "Draft PR metadata, successful devnet status, and workflow comments."],
  "transaction-trace.json": ["Transaction call trace", "chain", "Successful callTracer output whose result matches the same-block eth_call."],
  "transaction.json": ["Verification transaction", "chain", "Raw JSON-RPC transaction including the complete attestation calldata."],
  "workflow-summary.json": ["Devnet workflows", "workflow", "Initial run, corrected regenesis, prune, and successful retry evidence."],
  "zone-prover-host.sha256": ["Host binary checksum", "checksum", "SHA-256 checksum of the host-side registration verifier."],
  "zone-prover.eif.sha256": ["EIF SHA-256", "checksum", "SHA-256 checksum of the deployed enclave image file."],
  "zone-prover.eif.sha384": ["EIF SHA-384", "checksum", "SHA-384 checksum of the deployed enclave image file."],
  "zone-prover.eif.size": ["EIF size", "nitro", "Exact byte size of the deployed enclave image file."],
  "verify-evidence.mjs": ["Evidence verifier source", "verification", "Dependency-free verifier for chain linkage, ABI output, PCRs, registration digest, and signature."],
  "build-evidence.mjs": ["Evidence builder source", "verification", "Reproducible script that derives evidence.json and its checksummed manifest."],
  "index.html": ["Demo page", "presentation", "Static evidence presentation markup."],
  "app.js": ["Demo renderer", "presentation", "Browser-side evidence renderer and surface checks."],
  "styles.css": ["Demo styles", "presentation", "Static evidence page styles."],
};

const artifactOrder = [
  "evidence-verifier.txt",
  "verify-evidence.mjs",
  "receipt.json",
  "transaction.json",
  "transaction-trace.json",
  "eth-call-request.json",
  "eth-call-response.json",
  "decoded-precompile-output.json",
  "registration.json",
  "registration-bindings.json",
  "eif-build.json",
  "enclaves-current.json",
  "aws-instance.json",
  "workflow-summary.json",
  "tempo-pr.json",
  "tempo-image-inspect.json",
  "github-image-build.json",
];

const artifactFiles = [
  ...readdirSync(artifactDir)
    .filter((name) => !name.startsWith("."))
    .map((name) => ({ name, file: join(artifactDir, name), url: `./artifacts/${name}` })),
  ...["verify-evidence.mjs", "build-evidence.mjs", "index.html", "app.js", "styles.css"]
    .map((name) => ({ name, file: join(baseDir, name), url: `./${name}` })),
];
artifactFiles.sort((left, right) => {
  const leftIndex = artifactOrder.indexOf(left.name);
  const rightIndex = artifactOrder.indexOf(right.name);
  if (leftIndex !== -1 || rightIndex !== -1) {
    return (leftIndex === -1 ? Number.MAX_SAFE_INTEGER : leftIndex) -
      (rightIndex === -1 ? Number.MAX_SAFE_INTEGER : rightIndex);
  }
  return left.name.localeCompare(right.name);
});

const artifacts = artifactFiles.map(({ name, file, url }) => {
  const bytes = readFileSync(file);
  const [label, kind, description] = artifactMetadata[name] ?? [basename(name), "artifact", "Captured verification evidence."];
  return {
    label,
    kind,
    description,
    url,
    sha256: sha256(bytes),
    sizeBytes: statSync(file).size,
  };
});

const evidence = {
  schemaVersion: "1.0",
  verification: {
    status: "verified",
    performedAt: new Date(hexQuantity(block.timestamp) * 1000).toISOString(),
    summary:
      "The TIP-1090 precompile accepted an AWS NSM-signed attestation from the measured Zones prover enclave in block 89. The transaction trace and same-block replay returned identical verifier output; PCR0-2, public key, nonce, and registration challenge all match the captured build and runtime evidence.",
    bundleVerifier: {
      checksPassed: 26,
      hashedArtifacts: artifacts.length,
      command: "node ./verify-evidence.mjs",
      source: "./verify-evidence.mjs",
      report: "./artifacts/evidence-verifier.txt",
    },
  },
  trustAnchor: {
    name: "AWS Nitro Enclaves commercial-partition root certificate (G1)",
    encoding: "DER",
    sha256: "641a0321a3e244efe456463195d606317ed7cdcc3c1756e09893f3c68f79bb5b",
    pinnedInCommit: tempoPr.headRefOid,
    sourceUrl: `https://github.com/tempoxyz/tempo/blob/${tempoPr.headRefOid}/crates/precompiles/src/nitro_attestation/mod.rs#L28-L34`,
  },
  chain: {
    name: "Tempo devnet · PR #6786",
    chainId: hexQuantity(transaction.chainId),
    rpcUrl: devnetRpc.rpcUrl,
    explorerUrl: null,
    precompile: {
      tip: "TIP-1090",
      address: receipt.to,
      functionSelector: transaction.input.slice(0, 10),
      functionSignature: "verifyAttestation(bytes)",
    },
    transaction: {
      hash: transaction.hash,
      url: "./artifacts/receipt.json",
      status: receipt.status,
      blockNumber: hexQuantity(transaction.blockNumber),
      blockHash: transaction.blockHash,
      from: transaction.from,
      to: transaction.to,
      gasUsed: hexQuantity(receipt.gasUsed),
      effectiveGasPrice: receipt.effectiveGasPrice,
      feeToken: receipt.feeToken,
      inputSha256: with0x(sha256(inputBytes)),
      receiptArtifact: "./artifacts/receipt.json",
      transactionArtifact: "./artifacts/transaction.json",
      traceArtifact: "./artifacts/transaction-trace.json",
    },
    ethCall: {
      method: "eth_call",
      blockNumber: hexQuantity(ethCallRequest.params[1]),
      functionSelector: ethCallRequest.params[0].data.slice(0, 10),
      resultSha256: with0x(sha256(resultBytes)),
      rawResult: ethCallResponse.result,
      decoded: {
        moduleId: decoded[0],
        timestamp: attestationTimestamp,
        pcrs: decoded[2].map(([index, value]) => ({ index, value })),
        publicKey: decoded[3],
        userData: decoded[4],
        nonce: decoded[5],
        certificateSha256: decoded[6],
      },
      requestArtifact: "./artifacts/eth-call-request.json",
      responseArtifact: "./artifacts/eth-call-response.json",
    },
  },
  attestation: {
    documentSha256: with0x(sha256(attestationBytes)),
    sizeBytes: attestationBytes.length,
    moduleId: decoded[0],
    timestamp: attestationTimestamp,
    timestampIso: new Date(attestationTimestamp).toISOString(),
    digest: "SHA384",
    certificateSha256: decoded[6],
    publicKey: decoded[3],
    userData: decoded[4],
    nonce: decoded[5],
    registrationReportDigest: registration.digest,
    signer: registration.signer,
    requestId: registration.requestId,
    bindings: {
      publicKeyMatchesRegistration: bindings.publicKeyMatchesRegistration,
      nonceMatchesRegistration: bindings.nonceMatchesRegistration,
      userDataMatchesRegistrationChallenge: bindings.userDataMatchesRegistrationChallenge,
    },
    pcrs: [0, 1, 2].map((index) => ({
      index,
      expected: with0x(eifBuild.Measurements[`PCR${index}`]),
      attested: decoded[2].find(([decodedIndex]) => Number(decodedIndex) === index)?.[1],
    })),
  },
  aws: {
    accountId: "940932546236",
    region: "us-east-1",
    availabilityZone: awsInstance.Placement.AvailabilityZone,
    instanceId: awsInstance.InstanceId,
    instanceType: awsInstance.InstanceType,
    amiId: awsInstance.ImageId,
    state: awsInstance.State,
    enclave: {
      id: enclave.EnclaveID,
      attestedModuleId: decoded[0],
      moduleIdFormatNote: "NSM zero-pads the hexadecimal enclave suffix; Nitro CLI omits that leading zero.",
      state: enclave.State,
      flags: enclave.Flags,
      cid: enclave.EnclaveCID,
      cpuCount: enclave.NumberOfCPUs,
      memoryMiB: enclave.MemoryMiB,
      eifName: enclave.EnclaveName,
      eifSizeBytes: eifSize,
      eifSha384,
    },
  },
  devnet: {
    name: "devnet-pr-6786",
    namespace: "argo-workflows",
    rpcUrl: devnetRpc.rpcUrl,
    chainId: hexQuantity(devnetRpc.chainId),
    clientVersion: devnetRpc.clientVersion,
    activeFork: devnetRpc.forkSchedule.active,
    image: tempoImage.name,
    imageDigest: tempoImage.manifest.digest,
    runningPodCount: devnetPods.pods.length,
    podImageId: devnetPods.pods[0].containers[0].imageID,
    podSnapshotAt: devnetPods.capturedAt,
    headBlockAtCapture: hexQuantity(devnetRpc.headBlock),
    genesisUrl: "https://devnet-assets.tempoxyz.dev/devnet-pr-6786.json",
  },
  provenance: {
    tempo: {
      prNumber: tempoPr.number,
      prTitle: tempoPr.title,
      prUrl: tempoPr.url,
      branch: tempoPr.headRefName,
      commit: tempoPr.headRefOid,
      commitUrl: `https://github.com/tempoxyz/tempo/commit/${tempoPr.headRefOid}`,
      image: tempoImage.name,
      imageDigest: tempoImage.manifest.digest,
      imageBuildUrl: imageBuild.url,
    },
    zones: {
      repository: "tempoxyz/zones",
      repositoryUrl: "https://github.com/tempoxyz/zones",
      branch: "demo/tip1090-nitro-attestation",
      commit: "2241a40b41ca62b52b1d471233afd308a6f4a5d5",
      commitUrl: "https://github.com/tempoxyz/zones/commit/2241a40b41ca62b52b1d471233afd308a6f4a5d5",
      image: dockerImage.RepoTags[0],
      imageDigest: dockerImage.Id,
      enclaveCommand: dockerImage.Config.Cmd,
    },
    workflow: {
      name: successfulWorkflow.name,
      namespace: "argo-workflows",
      url: `https://dev-eu-tempo-workflows-ui.tail388b2e.ts.net/?page=workflows&ns=argo-workflows&wf=${successfulWorkflow.name}`,
      status: successfulWorkflow.phase,
      startedAt: successfulWorkflow.startedAt,
      finishedAt: successfulWorkflow.finishedAt,
      duration: `${Math.floor(workflowDurationSeconds / 60)}m ${workflowDurationSeconds % 60}s`,
      recoveryWorkflows: [workflowSummary.regenesis.name, workflowSummary.prune.name],
    },
  },
  limitations: [
    "This proves that the TIP-1090 code accepted an authentic AWS Nitro attestation and returned the recorded claims.",
    "PCR equality proves that the attested enclave matches the captured EIF build measurements; this demo does not establish an application-wide PCR allowlist policy.",
    "The registration binds the attested public key to its challenge and signer; this demo does not execute or prove a Zones batch.",
    "The EIF measurements identify this exact build, but Dockerfile.nitro-prover uses tag-referenced base images; rebuilding from source alone may not reproduce the same PCRs after those tags move.",
    "The receipt commits successful precompile acceptance. Decoded return bytes are corroborating RPC trace and same-block eth_call evidence, not data committed into the receipt.",
  ],
  artifacts,
  raw: {
    transaction: transactionResponse,
    transactionReceipt: receipt,
    transactionTrace,
    ethCallRequest,
    ethCallResponse,
    decodedPrecompileOutput: decodedOutput,
    devnetPods,
    registrationBindings: bindings,
    awsInstanceDescription: awsInstance,
    nitroEnclaveDescription: enclave,
    workflowSummary: {
      successfulRetry: workflowSummary.successfulRetry,
      regenesis: workflowSummary.regenesis,
      prune: workflowSummary.prune,
    },
    commands: [
      "node ./verify-evidence.mjs",
      "cast receipt $TX_HASH --rpc-url $RPC_URL --json",
      "cast rpc --rpc-url $RPC_URL debug_traceTransaction $TX_HASH '{\"tracer\":\"callTracer\"}'",
      "cast rpc --rpc-url $RPC_URL eth_call '{\"to\":\"$PRECOMPILE\",\"data\":\"$CALLDATA\"}' 0x59",
      "nitro-cli describe-enclaves",
      "sha384sum build/zone-prover.eif",
    ],
  },
};

writeFileSync(join(baseDir, "evidence.json"), `${JSON.stringify(evidence, null, 2)}\n`);
console.log(`wrote evidence.json with ${artifacts.length} hashed artifacts`);
