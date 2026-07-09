#!/usr/bin/env node

import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const VERIFY_ATTESTATION_SELECTOR = "0x769d87e7";
const NITRO_ATTESTATION_PRECOMPILE = "0xa77e570000000000000000000000000000000000";
const baseDir = dirname(fileURLToPath(import.meta.url));
const failures = [];
let passes = 0;

function pass(label, detail = "") {
  passes += 1;
  console.log(`PASS ${label}${detail ? ` — ${detail}` : ""}`);
}

function fail(label, error) {
  const message = error instanceof Error ? error.message : String(error);
  failures.push(`${label}: ${message}`);
  console.error(`FAIL ${label} — ${message}`);
}

function check(label, assertion) {
  try {
    const detail = assertion();
    pass(label, typeof detail === "string" ? detail : "");
  } catch (error) {
    fail(label, error);
  }
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function readJson(name, required = true) {
  const file = join(baseDir, name);
  if (!existsSync(file)) {
    if (!required) return null;
    throw new Error(`${name} is missing`);
  }

  let bytes;
  try {
    bytes = readFileSync(file);
  } catch (error) {
    throw new Error(`cannot read ${name}: ${error.message}`);
  }

  try {
    return { file, bytes, value: JSON.parse(bytes.toString("utf8")) };
  } catch (error) {
    throw new Error(`cannot parse ${name}: ${error.message}`);
  }
}

function requiredString(value, label) {
  assert(typeof value === "string" && value.length > 0, `${label} must be a non-empty string`);
  return value;
}

function hexBytes(value, label, expectedBytes) {
  const hex = requiredString(value, label);
  assert(/^0x[0-9a-fA-F]*$/.test(hex), `${label} must be 0x-prefixed hexadecimal`);
  assert((hex.length - 2) % 2 === 0, `${label} must contain whole bytes`);
  const bytes = Buffer.from(hex.slice(2), "hex");
  if (expectedBytes !== undefined) {
    assert(bytes.length === expectedBytes, `${label} must be ${expectedBytes} bytes, got ${bytes.length}`);
  }
  return bytes;
}

function normalizedHex(value, label, expectedBytes) {
  return hexBytes(value, label, expectedBytes).toString("hex");
}

function normalizedDigest(value, label) {
  const digest = requiredString(value, label).toLowerCase().replace(/^0x/, "");
  assert(/^[0-9a-f]{64}$/.test(digest), `${label} must be a SHA-256 digest`);
  return digest;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

const U64_MASK = (1n << 64n) - 1n;
const KECCAK_ROUNDS = [
  0x0000000000000001n, 0x0000000000008082n, 0x800000000000808an,
  0x8000000080008000n, 0x000000000000808bn, 0x0000000080000001n,
  0x8000000080008081n, 0x8000000000008009n, 0x000000000000008an,
  0x0000000000000088n, 0x0000000080008009n, 0x000000008000000an,
  0x000000008000808bn, 0x800000000000008bn, 0x8000000000008089n,
  0x8000000000008003n, 0x8000000000008002n, 0x8000000000000080n,
  0x000000000000800an, 0x800000008000000an, 0x8000000080008081n,
  0x8000000000008080n, 0x0000000080000001n, 0x8000000080008008n,
];
const KECCAK_ROTATIONS = [
  0, 1, 62, 28, 27,
  36, 44, 6, 55, 20,
  3, 10, 43, 25, 39,
  41, 45, 15, 21, 8,
  18, 2, 61, 56, 14,
];

function rotateLeft64(value, shift) {
  if (shift === 0) return value & U64_MASK;
  const amount = BigInt(shift);
  return ((value << amount) | (value >> (64n - amount))) & U64_MASK;
}

function keccakPermutation(state) {
  for (const round of KECCAK_ROUNDS) {
    const columns = Array(5).fill(0n);
    for (let x = 0; x < 5; x += 1) {
      for (let y = 0; y < 5; y += 1) columns[x] ^= state[x + 5 * y];
    }
    for (let x = 0; x < 5; x += 1) {
      const delta = columns[(x + 4) % 5] ^ rotateLeft64(columns[(x + 1) % 5], 1);
      for (let y = 0; y < 5; y += 1) state[x + 5 * y] = (state[x + 5 * y] ^ delta) & U64_MASK;
    }

    const moved = Array(25).fill(0n);
    for (let x = 0; x < 5; x += 1) {
      for (let y = 0; y < 5; y += 1) {
        moved[y + 5 * ((2 * x + 3 * y) % 5)] = rotateLeft64(
          state[x + 5 * y],
          KECCAK_ROTATIONS[x + 5 * y],
        );
      }
    }

    for (let x = 0; x < 5; x += 1) {
      for (let y = 0; y < 5; y += 1) {
        state[x + 5 * y] = (
          moved[x + 5 * y] ^
          ((~moved[((x + 1) % 5) + 5 * y]) & moved[((x + 2) % 5) + 5 * y])
        ) & U64_MASK;
      }
    }
    state[0] = (state[0] ^ round) & U64_MASK;
  }
}

function keccak256(bytes) {
  const rate = 136;
  const padding = rate - (bytes.length % rate);
  const padded = Buffer.alloc(bytes.length + padding);
  bytes.copy(padded);
  padded[bytes.length] = 0x01;
  padded[padded.length - 1] |= 0x80;

  const state = Array(25).fill(0n);
  for (let offset = 0; offset < padded.length; offset += rate) {
    for (let lane = 0; lane < rate / 8; lane += 1) {
      state[lane] ^= padded.readBigUInt64LE(offset + lane * 8);
    }
    keccakPermutation(state);
  }

  const result = Buffer.alloc(32);
  for (let lane = 0; lane < 4; lane += 1) result.writeBigUInt64LE(state[lane], lane * 8);
  return result;
}

function leftPad32(bytes, label) {
  assert(bytes.length <= 32, `${label} exceeds one ABI word`);
  const word = Buffer.alloc(32);
  bytes.copy(word, 32 - bytes.length);
  return word;
}

function unsignedWord(value, label) {
  const integerValue = integer(value, label);
  return leftPad32(Buffer.from(integerValue.toString(16).padStart(2, "0"), "hex"), label);
}

function registrationDigests(report) {
  const publicKey = hexBytes(report.publicKeyUncompressed, "registration.publicKeyUncompressed", 65);
  const document = hexBytes(report.attestationDoc, "registration.attestationDoc");
  const fields = [
    keccak256(Buffer.from("tempo.zone.prover.registration.v1")),
    unsignedWord(report.version, "registration.version"),
    leftPad32(hexBytes(report.requestId, "registration.requestId", 32), "registration.requestId"),
    leftPad32(hexBytes(report.nonce, "registration.nonce", 32), "registration.nonce"),
    leftPad32(hexBytes(report.signer, "registration.signer", 20), "registration.signer"),
    keccak256(publicKey),
    keccak256(hexBytes(report.expectedPcr0, "registration.expectedPcr0", 48)),
    keccak256(hexBytes(report.expectedPcr1, "registration.expectedPcr1", 48)),
    keccak256(hexBytes(report.expectedPcr2, "registration.expectedPcr2", 48)),
  ];
  return {
    challenge: keccak256(Buffer.concat([...fields, Buffer.alloc(32)])),
    report: keccak256(Buffer.concat([...fields, keccak256(document)])),
  };
}

const SECP256K1_P = 0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2fn;
const SECP256K1_N = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;
const SECP256K1_G = {
  x: 0x79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798n,
  y: 0x483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8n,
};

function modulo(value, modulus) {
  const remainder = value % modulus;
  return remainder >= 0n ? remainder : remainder + modulus;
}

function inverse(value, modulus) {
  let oldR = modulo(value, modulus);
  let r = modulus;
  let oldS = 1n;
  let s = 0n;
  while (r !== 0n) {
    const quotient = oldR / r;
    [oldR, r] = [r, oldR - quotient * r];
    [oldS, s] = [s, oldS - quotient * s];
  }
  assert(oldR === 1n, "value has no modular inverse");
  return modulo(oldS, modulus);
}

function pointAdd(left, right) {
  if (!left) return right;
  if (!right) return left;
  if (left.x === right.x && modulo(left.y + right.y, SECP256K1_P) === 0n) return null;
  const slope = left.x === right.x && left.y === right.y
    ? modulo(3n * left.x * left.x * inverse(2n * left.y, SECP256K1_P), SECP256K1_P)
    : modulo((right.y - left.y) * inverse(right.x - left.x, SECP256K1_P), SECP256K1_P);
  const x = modulo(slope * slope - left.x - right.x, SECP256K1_P);
  const y = modulo(slope * (left.x - x) - left.y, SECP256K1_P);
  return { x, y };
}

function pointMultiply(scalar, point) {
  let multiplier = modulo(scalar, SECP256K1_N);
  let addend = point;
  let result = null;
  while (multiplier > 0n) {
    if (multiplier & 1n) result = pointAdd(result, addend);
    addend = pointAdd(addend, addend);
    multiplier >>= 1n;
  }
  return result;
}

function verifyRegistrationSignature(report, digest) {
  const publicKey = hexBytes(report.publicKeyUncompressed, "registration.publicKeyUncompressed", 65);
  assert(publicKey[0] === 4, "registration public key is not uncompressed SEC1");
  const key = {
    x: BigInt(`0x${publicKey.subarray(1, 33).toString("hex")}`),
    y: BigInt(`0x${publicKey.subarray(33).toString("hex")}`),
  };
  assert(
    modulo(key.y * key.y - (key.x * key.x * key.x + 7n), SECP256K1_P) === 0n,
    "registration public key is not on secp256k1",
  );

  const signature = hexBytes(report.signature, "registration.signature", 65);
  const r = BigInt(`0x${signature.subarray(0, 32).toString("hex")}`);
  const s = BigInt(`0x${signature.subarray(32, 64).toString("hex")}`);
  assert(r > 0n && r < SECP256K1_N && s > 0n && s < SECP256K1_N, "registration signature scalar is out of range");
  assert(signature[64] <= 1, "registration recovery ID is not 0 or 1");

  const z = BigInt(`0x${digest.toString("hex")}`);
  const inverseS = inverse(s, SECP256K1_N);
  const point = pointAdd(
    pointMultiply(modulo(z * inverseS, SECP256K1_N), SECP256K1_G),
    pointMultiply(modulo(r * inverseS, SECP256K1_N), key),
  );
  assert(point && modulo(point.x, SECP256K1_N) === r, "registration signature is invalid");

  const address = keccak256(publicKey.subarray(1)).subarray(12);
  equalBytes(`0x${address.toString("hex")}`, report.signer, "public-key-derived signer", "registration.signer", 20);
}

function equalBytes(left, right, leftLabel, rightLabel, expectedBytes) {
  const a = normalizedHex(left, leftLabel, expectedBytes);
  const b = normalizedHex(right, rightLabel, expectedBytes);
  assert(a === b, `${leftLabel} does not equal ${rightLabel}`);
}

function integer(value, label) {
  assert(value !== null && value !== undefined && value !== "", `${label} is missing`);
  try {
    if (typeof value === "number") {
      assert(Number.isSafeInteger(value) && value >= 0, `${label} must be a non-negative safe integer`);
    }
    const result = BigInt(value);
    assert(result >= 0n, `${label} must be non-negative`);
    return result;
  } catch (error) {
    if (error.message?.startsWith(`${label} `)) throw error;
    throw new Error(`${label} is not an integer`);
  }
}

function successfulStatus(value) {
  if (value === true || value === 1 || value === 1n) return true;
  return ["0x1", "1", "success", "succeeded", "passed"].includes(String(value ?? "").toLowerCase());
}

function unwrapResult(value) {
  return value && typeof value === "object" && value.result && typeof value.result === "object"
    ? value.result
    : value;
}

function registrationReport(value) {
  const unwrapped = unwrapResult(value);
  return unwrapped?.registrationReport ?? unwrapped?.registration ?? unwrapped?.report ?? unwrapped;
}

function transactionRecord(value) {
  const unwrapped = unwrapResult(value);
  return unwrapped?.transaction ?? unwrapped;
}

function firstString(candidates, label) {
  const values = candidates.filter((value) => value !== null && value !== undefined && value !== "");
  assert(values.length > 0, `${label} is missing`);
  const first = requiredString(values[0], label);
  for (const value of values.slice(1)) {
    assert(requiredString(value, label).toLowerCase() === first.toLowerCase(), `${label} sources disagree`);
  }
  return first;
}

function normalizedEnclaveId(value, label) {
  const id = requiredString(value, label).toLowerCase();
  const match = id.match(/^(i-[0-9a-f]+-enc)([0-9a-f]+)$/);
  assert(match, `${label} is not an AWS enclave ID`);
  return `${match[1]}${match[2].replace(/^0+(?=[0-9a-f])/, "")}`;
}

function parseVerifyAttestationCalldata(input) {
  const calldata = hexBytes(input, "transaction input");
  assert(calldata.length >= 4 + 64, "transaction input is too short for verifyAttestation(bytes)");
  assert(`0x${calldata.subarray(0, 4).toString("hex")}` === VERIFY_ATTESTATION_SELECTOR, "transaction input selector is not verifyAttestation(bytes)");

  const argumentsData = calldata.subarray(4);
  const offset = wordAsSafeInteger(argumentsData.subarray(0, 32), "attestation argument offset");
  assert(offset === 32, `attestation argument has non-canonical offset ${offset}`);
  assert(offset + 32 <= argumentsData.length, "attestation argument length word is out of bounds");

  const length = wordAsSafeInteger(argumentsData.subarray(offset, offset + 32), "attestation document length");
  const start = offset + 32;
  const paddedLength = Math.ceil(length / 32) * 32;
  const end = start + length;
  const paddedEnd = start + paddedLength;
  assert(end <= argumentsData.length, "attestation document is truncated in transaction input");
  assert(paddedEnd === argumentsData.length, "transaction input has missing padding or trailing bytes");
  assert(argumentsData.subarray(end, paddedEnd).every((byte) => byte === 0), "attestation document has non-zero ABI padding");

  return { calldata, document: argumentsData.subarray(start, end) };
}

function wordAsSafeInteger(word, label) {
  assert(word.length === 32, `${label} is truncated`);
  const value = BigInt(`0x${word.toString("hex")}`);
  assert(value <= BigInt(Number.MAX_SAFE_INTEGER), `${label} exceeds the safe integer range`);
  return Number(value);
}

function abiWord(data, offset, label) {
  assert(Number.isSafeInteger(offset) && offset >= 0, `${label} has an invalid offset`);
  assert(offset + 32 <= data.length, `${label} is out of bounds`);
  return data.subarray(offset, offset + 32);
}

function abiDynamicBytes(data, offset, label) {
  const length = wordAsSafeInteger(abiWord(data, offset, `${label} length`), `${label} length`);
  const start = offset + 32;
  const paddedEnd = start + Math.ceil(length / 32) * 32;
  assert(paddedEnd <= data.length, `${label} is truncated`);
  assert(data.subarray(start + length, paddedEnd).every((byte) => byte === 0), `${label} has non-zero padding`);
  return data.subarray(start, start + length);
}

function decodeAttestationResult(rawResult) {
  const data = hexBytes(rawResult, "eth_call raw result");
  assert(data.length >= 32, "eth_call raw result is truncated");
  const tupleBase = wordAsSafeInteger(abiWord(data, 0, "return tuple offset"), "return tuple offset");
  assert(tupleBase === 32, `return tuple has non-canonical offset ${tupleBase}`);
  assert(tupleBase + 7 * 32 <= data.length, "return tuple head is truncated");

  const dynamicFromTuple = (headIndex, label) => {
    const relative = wordAsSafeInteger(
      abiWord(data, tupleBase + headIndex * 32, `${label} offset`),
      `${label} offset`,
    );
    assert(relative % 32 === 0 && relative >= 7 * 32, `${label} has a non-canonical offset`);
    return abiDynamicBytes(data, tupleBase + relative, label);
  };

  const moduleBytes = dynamicFromTuple(0, "moduleId");
  let moduleId;
  try {
    moduleId = new TextDecoder("utf-8", { fatal: true }).decode(moduleBytes);
  } catch {
    throw new Error("moduleId is not valid UTF-8");
  }

  const timestampWord = abiWord(data, tupleBase + 32, "timestamp");
  assert(timestampWord.subarray(0, 24).every((byte) => byte === 0), "timestamp exceeds uint64");
  const timestamp = wordAsSafeInteger(timestampWord, "timestamp");

  const pcrRelative = wordAsSafeInteger(
    abiWord(data, tupleBase + 2 * 32, "PCR array offset"),
    "PCR array offset",
  );
  assert(pcrRelative % 32 === 0 && pcrRelative >= 7 * 32, "PCR array has a non-canonical offset");
  const pcrArray = tupleBase + pcrRelative;
  const pcrCount = wordAsSafeInteger(abiWord(data, pcrArray, "PCR array length"), "PCR array length");
  assert(pcrCount <= 32, `PCR array has unreasonable length ${pcrCount}`);
  const offsetsBase = pcrArray + 32;
  assert(offsetsBase + pcrCount * 32 <= data.length, "PCR array offsets are truncated");

  const pcrs = [];
  for (let position = 0; position < pcrCount; position += 1) {
    const relative = wordAsSafeInteger(
      abiWord(data, offsetsBase + position * 32, `PCR${position} tuple offset`),
      `PCR${position} tuple offset`,
    );
    assert(relative % 32 === 0 && relative >= pcrCount * 32, `PCR${position} has a non-canonical tuple offset`);
    const tuple = offsetsBase + relative;
    const indexWord = abiWord(data, tuple, `PCR${position} index`);
    assert(indexWord.subarray(0, 31).every((byte) => byte === 0), `PCR${position} index exceeds uint8`);
    const valueRelative = wordAsSafeInteger(
      abiWord(data, tuple + 32, `PCR${position} value offset`),
      `PCR${position} value offset`,
    );
    assert(valueRelative === 64, `PCR${position} value has non-canonical offset ${valueRelative}`);
    const value = abiDynamicBytes(data, tuple + valueRelative, `PCR${position} value`);
    pcrs.push([indexWord[31], `0x${value.toString("hex")}`]);
  }

  return [
    moduleId,
    timestamp,
    pcrs,
    `0x${dynamicFromTuple(3, "publicKey").toString("hex")}`,
    `0x${dynamicFromTuple(4, "userData").toString("hex")}`,
    `0x${dynamicFromTuple(5, "nonce").toString("hex")}`,
    `0x${abiWord(data, tupleBase + 6 * 32, "leaf certificate hash").toString("hex")}`,
  ];
}

function localArtifactPath(url, label) {
  const rawUrl = requiredString(url, `${label}.url`);
  if (/^[a-zA-Z][a-zA-Z\d+.-]*:/.test(rawUrl) || rawUrl.startsWith("//") || rawUrl.startsWith("/")) {
    return null;
  }

  const pathname = rawUrl.split(/[?#]/, 1)[0];
  assert(pathname.length > 0, `${label}.url has no file path`);
  let decoded;
  try {
    decoded = decodeURIComponent(pathname);
  } catch {
    throw new Error(`${label}.url has invalid percent encoding`);
  }
  assert(!isAbsolute(decoded), `${label}.url must not be an absolute filesystem path`);

  const file = resolve(baseDir, decoded);
  const fromBase = relative(baseDir, file);
  assert(fromBase !== ".." && !fromBase.startsWith(`..${sep}`) && !isAbsolute(fromBase), `${label}.url escapes the demo directory`);
  return file;
}

let evidenceFile;
let registrationFile;
let transactionFile;
let transactionTraceFile;
let receiptFile;
let blockFile;
let ethCallRequestFile;
let ethCallResponseFile;
let decodedOutputFile;
let eifBuildFile;
let enclavesFile;
let registrationBindingsFile;
let devnetPodsFile;

try {
  evidenceFile = readJson("evidence.json");
  registrationFile = readJson("artifacts/registration.json");
  transactionFile = readJson("artifacts/transaction.json");
  transactionTraceFile = readJson("artifacts/transaction-trace.json");
  receiptFile = readJson("artifacts/receipt.json");
  blockFile = readJson("artifacts/block.json");
  ethCallRequestFile = readJson("artifacts/eth-call-request.json");
  ethCallResponseFile = readJson("artifacts/eth-call-response.json");
  decodedOutputFile = readJson("artifacts/decoded-precompile-output.json");
  eifBuildFile = readJson("artifacts/eif-build.json");
  enclavesFile = readJson("artifacts/enclaves-current.json");
  registrationBindingsFile = readJson("artifacts/registration-bindings.json");
  devnetPodsFile = readJson("artifacts/devnet-pods.json");
} catch (error) {
  fail("load evidence files", error);
  console.error(`FAILED 0 checks passed; ${failures.length} failed`);
  process.exit(1);
}

const evidence = evidenceFile.value;
const registration = registrationReport(registrationFile.value);
const artifactTransaction = transactionRecord(transactionFile.value);
const transactionTrace = unwrapResult(transactionTraceFile.value);
const artifactReceipt = unwrapResult(receiptFile.value);
const artifactBlock = unwrapResult(blockFile.value);
const ethCallRequest = ethCallRequestFile.value;
const ethCallResponse = ethCallResponseFile.value;
const decodedOutput = decodedOutputFile.value;
const eifMeasurements = eifBuildFile.value?.Measurements;
const currentEnclaves = enclavesFile.value;
const registrationBindings = registrationBindingsFile.value;
const devnetPods = devnetPodsFile.value;
const chainTransaction = evidence?.chain?.transaction;
const ethCall = evidence?.chain?.ethCall;
const precompile = evidence?.chain?.precompile;
const attestation = evidence?.attestation;
const decoded = ethCall?.decoded;

let transactionInput;
let parsedCalldata;
let registrationDocument;
let registrationDigestValues;

check("Keccak-256 implementation", () => {
  const emptyDigest = keccak256(Buffer.alloc(0)).toString("hex");
  assert(
    emptyDigest === "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470",
    "Keccak-256 empty-string test vector failed",
  );
});

check("verification status", () => {
  assert(String(evidence?.verification?.status ?? "").toLowerCase() === "verified", "verification.status must be verified");
});

check("successful transaction to precompile", () => {
  assert(chainTransaction && precompile, "chain transaction or precompile evidence is missing");
  assert(successfulStatus(chainTransaction.status), `transaction status is not successful: ${chainTransaction.status ?? "missing"}`);
  equalBytes(precompile.address, NITRO_ATTESTATION_PRECOMPILE, "precompile.address", "TIP-1090 precompile address", 20);
  equalBytes(chainTransaction.to, precompile.address, "transaction.to", "precompile.address", 20);
  normalizedHex(chainTransaction.hash, "transaction.hash", 32);
  if (artifactTransaction?.to) equalBytes(artifactTransaction.to, chainTransaction.to, "artifact transaction.to", "transaction.to", 20);
  if (artifactTransaction?.hash) equalBytes(artifactTransaction.hash, chainTransaction.hash, "artifact transaction.hash", "transaction.hash", 32);
  equalBytes(artifactReceipt?.transactionHash, chainTransaction.hash, "receipt.transactionHash", "transaction.hash", 32);
  equalBytes(artifactReceipt?.to, chainTransaction.to, "receipt.to", "transaction.to", 20);
  assert(successfulStatus(artifactReceipt?.status), `receipt status is not successful: ${artifactReceipt?.status ?? "missing"}`);
  return chainTransaction.hash;
});

check("transaction and eth_call block", () => {
  const txBlock = integer(chainTransaction?.blockNumber, "transaction.blockNumber");
  const callBlock = integer(ethCall?.blockNumber, "ethCall.blockNumber");
  assert(txBlock === callBlock, `block mismatch: transaction ${txBlock}, eth_call ${callBlock}`);
  if (artifactTransaction?.blockNumber !== undefined) {
    assert(integer(artifactTransaction.blockNumber, "artifact transaction.blockNumber") === txBlock, "artifact transaction block disagrees with evidence");
  }
  assert(integer(artifactReceipt?.blockNumber, "receipt.blockNumber") === txBlock, "receipt block disagrees with evidence");
  assert(integer(artifactBlock?.number, "block.number") === txBlock, "block artifact number disagrees with evidence");
  equalBytes(artifactTransaction?.blockHash, chainTransaction?.blockHash, "artifact transaction.blockHash", "transaction.blockHash", 32);
  equalBytes(artifactReceipt?.blockHash, chainTransaction?.blockHash, "receipt.blockHash", "transaction.blockHash", 32);
  equalBytes(artifactBlock?.hash, chainTransaction?.blockHash, "block.hash", "transaction.blockHash", 32);
  assert(Array.isArray(artifactBlock?.transactions), "block.transactions is missing");
  const blockTransactions = artifactBlock.transactions.map((item) => typeof item === "string" ? item : item?.hash);
  assert(
    blockTransactions.some((hash) => typeof hash === "string" && hash.toLowerCase() === chainTransaction.hash.toLowerCase()),
    "verification transaction is not present in the captured block",
  );
  return `block ${txBlock}`;
});

check("verifyAttestation selector", () => {
  equalBytes(precompile?.functionSelector, VERIFY_ATTESTATION_SELECTOR, "precompile.functionSelector", "verifyAttestation selector", 4);
  equalBytes(ethCall?.functionSelector, VERIFY_ATTESTATION_SELECTOR, "ethCall.functionSelector", "verifyAttestation selector", 4);
  if (precompile?.functionSignature !== undefined) {
    assert(precompile.functionSignature === "verifyAttestation(bytes)", "precompile.functionSignature is not verifyAttestation(bytes)");
  }
  return VERIFY_ATTESTATION_SELECTOR;
});

check("same-block eth_call request and response", () => {
  assert(ethCallRequest?.method === "eth_call", "eth-call-request method is not eth_call");
  assert(Array.isArray(ethCallRequest?.params) && ethCallRequest.params.length === 2, "eth-call-request params are invalid");
  assert(ethCallResponse?.error === undefined || ethCallResponse.error === null, "eth-call-response contains an error");
  equalBytes(ethCallRequest.params[0]?.to, precompile?.address, "eth_call request.to", "precompile.address", 20);
  equalBytes(ethCallRequest.params[0]?.data, artifactTransaction?.input, "eth_call request.data", "transaction.input");
  assert(
    integer(ethCallRequest.params[1], "eth_call request block") === integer(chainTransaction?.blockNumber, "transaction.blockNumber"),
    "eth_call request did not use the transaction block",
  );
  equalBytes(ethCallResponse?.result, ethCall?.rawResult, "eth_call response.result", "ethCall.rawResult");
  return `block ${integer(ethCallRequest.params[1], "eth_call request block")}`;
});

check("ABI decode of raw precompile output", () => {
  assert(Array.isArray(decodedOutput) && decodedOutput.length === 1, "decoded output artifact must contain one return tuple");
  const abiDecoded = decodeAttestationResult(ethCall?.rawResult);
  const artifactDecoded = decodedOutput[0];
  assert(Array.isArray(artifactDecoded) && artifactDecoded.length === 7, "decoded output artifact has the wrong tuple shape");
  assert(abiDecoded[0] === artifactDecoded[0], "ABI-decoded moduleId disagrees with decoded artifact");
  assert(integer(abiDecoded[1], "ABI timestamp") === integer(artifactDecoded[1], "artifact timestamp"), "ABI-decoded timestamp disagrees with decoded artifact");
  assert(abiDecoded[0] === decoded?.moduleId, "ABI-decoded moduleId disagrees with evidence");
  assert(integer(abiDecoded[1], "ABI timestamp") === integer(decoded?.timestamp, "decoded.timestamp"), "ABI-decoded timestamp disagrees with evidence");

  assert(Array.isArray(abiDecoded[2]) && Array.isArray(artifactDecoded[2]), "decoded PCR arrays are missing");
  assert(abiDecoded[2].length === artifactDecoded[2].length, "ABI-decoded PCR count disagrees with decoded artifact");
  assert(abiDecoded[2].length === decoded?.pcrs?.length, "ABI-decoded PCR count disagrees with evidence");
  for (let position = 0; position < abiDecoded[2].length; position += 1) {
    const [abiIndex, abiValue] = abiDecoded[2][position];
    const [artifactIndex, artifactValue] = artifactDecoded[2][position];
    const evidencePcr = decoded.pcrs[position];
    assert(Number(abiIndex) === Number(artifactIndex) && Number(abiIndex) === Number(evidencePcr?.index), `PCR position ${position} index mismatch`);
    equalBytes(abiValue, artifactValue, `ABI PCR${abiIndex}`, `artifact PCR${artifactIndex}`, 48);
    equalBytes(abiValue, evidencePcr?.value, `ABI PCR${abiIndex}`, `evidence PCR${evidencePcr?.index}`, 48);
  }

  const labels = ["publicKey", "userData", "nonce", "certificateSha256"];
  for (let position = 3; position <= 6; position += 1) {
    const label = labels[position - 3];
    const expectedBytes = position === 3 ? 65 : 32;
    equalBytes(abiDecoded[position], artifactDecoded[position], `ABI ${label}`, `artifact ${label}`, expectedBytes);
    equalBytes(abiDecoded[position], decoded?.[label], `ABI ${label}`, `evidence ${label}`, expectedBytes);
  }
  return `${abiDecoded[2].length} PCRs and 4 bound fields`;
});

for (const index of [0, 1, 2]) {
  check(`PCR${index} match`, () => {
    const matches = Array.isArray(attestation?.pcrs)
      ? attestation.pcrs.filter((entry) => Number(entry?.index) === index)
      : [];
    assert(matches.length === 1, `expected exactly one PCR${index} record`);
    const entry = matches[0];
    equalBytes(entry.expected, entry.attested, `PCR${index}.expected`, `PCR${index}.attested`, 48);
    const registrationPcr = registration?.[`expectedPcr${index}`];
    equalBytes(registrationPcr, entry.attested, `registration.expectedPcr${index}`, `PCR${index}.attested`, 48);
    equalBytes(`0x${eifMeasurements?.[`PCR${index}`] ?? ""}`, entry.attested, `EIF build PCR${index}`, `PCR${index}.attested`, 48);
    assert(Array.isArray(currentEnclaves) && currentEnclaves.length === 1, "expected exactly one running enclave");
    equalBytes(
      `0x${currentEnclaves[0]?.Measurements?.[`PCR${index}`] ?? ""}`,
      entry.attested,
      `running enclave PCR${index}`,
      `PCR${index}.attested`,
      48,
    );
  });
}

check("non-debug enclave runtime", () => {
  assert(Array.isArray(currentEnclaves) && currentEnclaves.length === 1, "expected exactly one enclave in the runtime capture");
  const enclave = currentEnclaves[0];
  assert(String(enclave?.State ?? "").toUpperCase() === "RUNNING", `enclave is not RUNNING: ${enclave?.State ?? "missing"}`);
  assert(String(enclave?.Flags ?? "").toUpperCase() === "NONE", `enclave is not non-debug: Flags=${enclave?.Flags ?? "missing"}`);
  return `${enclave.EnclaveID}, Flags=${enclave.Flags}`;
});

check("devnet pod image provenance", () => {
  assert(devnetPods?.namespace === evidence?.devnet?.name, "devnet pod namespace disagrees with evidence");
  assert(Array.isArray(devnetPods?.pods) && devnetPods.pods.length > 0, "devnet pod snapshot is empty");
  assert(integer(evidence?.devnet?.runningPodCount, "devnet.runningPodCount") === BigInt(devnetPods.pods.length), "devnet running pod count disagrees with snapshot");
  const expectedImage = requiredString(evidence?.devnet?.image, "devnet.image");
  const expectedDigest = requiredString(evidence?.devnet?.imageDigest, "devnet.imageDigest").toLowerCase();
  for (const pod of devnetPods.pods) {
    assert(pod?.phase === "Running", `pod ${pod?.name ?? "unknown"} is not Running`);
    assert(Array.isArray(pod?.containers) && pod.containers.length > 0, `pod ${pod?.name ?? "unknown"} has no container status`);
    for (const container of pod.containers) {
      assert(container.image === expectedImage, `pod ${pod.name} image tag disagrees with evidence`);
      assert(container.ready === true, `pod ${pod.name} container ${container.name} is not ready`);
      assert(Number(container.restartCount) === 0, `pod ${pod.name} container ${container.name} restarted`);
      const imageId = requiredString(container.imageID, `pod ${pod.name} imageID`).toLowerCase();
      assert(imageId.endsWith(`@${expectedDigest}`), `pod ${pod.name} imageID does not match ${expectedDigest}`);
    }
  }
  return `${devnetPods.pods.length} running pods at ${expectedDigest}`;
});

check("decoded module ID", () => {
  const expected = requiredString(attestation?.moduleId, "attestation.moduleId");
  assert(requiredString(decoded?.moduleId, "decoded.moduleId") === expected, "decoded.moduleId does not equal attestation.moduleId");
  assert(
    normalizedEnclaveId(evidence?.aws?.enclave?.id, "aws.enclave.id") === normalizedEnclaveId(expected, "attestation.moduleId"),
    "Nitro CLI enclave ID and attested moduleId do not identify the same enclave",
  );
});

check("decoded public key", () => {
  equalBytes(decoded?.publicKey, attestation?.publicKey, "decoded.publicKey", "attestation.publicKey", 65);
  equalBytes(registration?.publicKeyUncompressed, attestation?.publicKey, "registration.publicKeyUncompressed", "attestation.publicKey", 65);
});

check("decoded user data", () => {
  equalBytes(decoded?.userData, attestation?.userData, "decoded.userData", "attestation.userData", 32);
});

check("decoded nonce", () => {
  equalBytes(decoded?.nonce, attestation?.nonce, "decoded.nonce", "attestation.nonce", 32);
  equalBytes(registration?.nonce, attestation?.nonce, "registration.nonce", "attestation.nonce", 32);
});

check("decoded certificate hash", () => {
  equalBytes(decoded?.certificateSha256, attestation?.certificateSha256, "decoded.certificateSha256", "attestation.certificateSha256", 32);
});

check("registration bindings", () => {
  const bindings = attestation?.bindings;
  assert(bindings?.publicKeyMatchesRegistration === true, "public key registration binding is not true");
  assert(bindings?.nonceMatchesRegistration === true, "nonce registration binding is not true");
  assert(bindings?.userDataMatchesRegistrationChallenge === true, "user-data registration challenge binding is not true");
  assert(registrationBindings?.publicKeyMatchesRegistration === true, "captured public key binding is not true");
  assert(registrationBindings?.nonceMatchesRegistration === true, "captured nonce binding is not true");
  assert(registrationBindings?.userDataMatchesRegistrationChallenge === true, "captured challenge binding is not true");
  equalBytes(registrationBindings?.registrationPublicKey, registration?.publicKeyUncompressed, "binding registration public key", "registration.publicKeyUncompressed", 65);
  equalBytes(registrationBindings?.attestedPublicKey, decoded?.publicKey, "binding attested public key", "decoded.publicKey", 65);
  equalBytes(registrationBindings?.registrationNonce, registration?.nonce, "binding registration nonce", "registration.nonce", 32);
  equalBytes(registrationBindings?.attestedNonce, decoded?.nonce, "binding attested nonce", "decoded.nonce", 32);
});

check("registration challenge and report digest", () => {
  assert(integer(registration?.version, "registration.version") === 1n, "registration version is not 1");
  registrationDigestValues = registrationDigests(registration);
  equalBytes(`0x${registrationDigestValues.challenge.toString("hex")}`, registrationBindings?.expectedUserData, "recomputed registration challenge", "binding expectedUserData", 32);
  equalBytes(`0x${registrationDigestValues.challenge.toString("hex")}`, registrationBindings?.attestedUserData, "recomputed registration challenge", "binding attestedUserData", 32);
  equalBytes(`0x${registrationDigestValues.challenge.toString("hex")}`, decoded?.userData, "recomputed registration challenge", "decoded.userData", 32);
  equalBytes(`0x${registrationDigestValues.report.toString("hex")}`, registration?.digest, "recomputed registration digest", "registration.digest", 32);
  return `challenge 0x${registrationDigestValues.challenge.toString("hex")}`;
});

check("registration secp256k1 signature and signer", () => {
  assert(registrationDigestValues, "registration digest was not recomputed");
  verifyRegistrationSignature(registration, registrationDigestValues.report);
  return registration.signer;
});

check("registration attestation document", () => {
  registrationDocument = hexBytes(registration?.attestationDoc, "registration.attestationDoc");
  assert(registrationDocument.length > 0, "registration.attestationDoc is empty");
  const actualHash = sha256(registrationDocument);
  const expectedHash = normalizedDigest(attestation?.documentSha256, "attestation.documentSha256");
  assert(actualHash === expectedHash, "registration attestationDoc SHA-256 does not match evidence");
  assert(integer(attestation?.sizeBytes, "attestation.sizeBytes") === BigInt(registrationDocument.length), "registration attestationDoc size does not match evidence");
  return `${registrationDocument.length} bytes, ${actualHash}`;
});

check("transaction calldata document", () => {
  transactionInput = firstString(
    [
      artifactTransaction?.input,
      chainTransaction?.input,
      evidence?.raw?.transaction?.input,
      evidence?.raw?.transactionReceipt?.input,
    ],
    "transaction input",
  );
  parsedCalldata = parseVerifyAttestationCalldata(transactionInput);
  assert(registrationDocument, "registration attestation document was not validated");
  assert(parsedCalldata.document.equals(registrationDocument), "verifyAttestation calldata contains a different attestation document");
  return `${parsedCalldata.document.length} document bytes`;
});

check("successful transaction trace", () => {
  assert(transactionTraceFile.value?.error === undefined || transactionTraceFile.value.error === null, "transaction trace JSON-RPC response has an error");
  assert(transactionTrace && typeof transactionTrace === "object", "transaction trace result is missing");
  assert(transactionTrace.error === undefined || transactionTrace.error === null || transactionTrace.error === "", `transaction trace failed: ${transactionTrace.error}`);
  assert(transactionTrace.revertReason === undefined || transactionTrace.revertReason === null || transactionTrace.revertReason === "", `transaction trace reverted: ${transactionTrace.revertReason}`);
  assert(String(transactionTrace.type ?? "").toUpperCase() === "CALL", `transaction trace type is not CALL: ${transactionTrace.type ?? "missing"}`);

  equalBytes(transactionTrace.from, artifactTransaction?.from, "transaction trace.from", "artifact transaction.from", 20);
  equalBytes(transactionTrace.to, artifactTransaction?.to, "transaction trace.to", "artifact transaction.to", 20);
  equalBytes(transactionTrace.input, artifactTransaction?.input, "transaction trace.input", "artifact transaction.input");

  const traceOutput = hexBytes(transactionTrace.output, "transaction trace.output");
  const callOutput = hexBytes(ethCall?.rawResult, "ethCall.rawResult");
  assert(traceOutput.length > 0, "transaction trace.output is empty");
  assert(traceOutput.equals(callOutput), "transaction trace.output does not equal ethCall.rawResult");

  const traceGasUsed = integer(transactionTrace.gasUsed, "transaction trace.gasUsed");
  const receiptGasUsed = integer(artifactReceipt?.gasUsed, "receipt.gasUsed");
  const evidenceGasUsed = integer(chainTransaction?.gasUsed, "transaction.gasUsed");
  assert(traceGasUsed === receiptGasUsed, "transaction trace gasUsed does not equal receipt gasUsed");
  assert(traceGasUsed === evidenceGasUsed, "transaction trace gasUsed does not equal evidence gasUsed");
  return `${traceGasUsed} gas`;
});

check("transaction input SHA-256", () => {
  assert(parsedCalldata, "transaction calldata was not validated");
  const actual = sha256(parsedCalldata.calldata);
  const expected = normalizedDigest(chainTransaction?.inputSha256, "transaction.inputSha256");
  assert(actual === expected, "transaction input SHA-256 does not match evidence");
  return actual;
});

check("eth_call result SHA-256", () => {
  const result = hexBytes(ethCall?.rawResult, "ethCall.rawResult");
  assert(result.length > 0, "ethCall.rawResult is empty");
  const actual = sha256(result);
  const expected = normalizedDigest(ethCall?.resultSha256, "ethCall.resultSha256");
  assert(actual === expected, "eth_call result SHA-256 does not match evidence");
  return actual;
});

check("relative artifact hashes", () => {
  assert(Array.isArray(evidence?.artifacts), "artifacts must be an array");
  let validated = 0;
  for (const [index, artifact] of evidence.artifacts.entries()) {
    assert(artifact && typeof artifact === "object", `artifacts[${index}] must be an object`);
    const label = `artifacts[${index}]`;
    const file = localArtifactPath(artifact.url, label);
    if (!file) continue;
    assert(existsSync(file), `${label} file does not exist`);
    const bytes = readFileSync(file);
    const actual = sha256(bytes);
    const expected = normalizedDigest(artifact.sha256, `${label}.sha256`);
    assert(actual === expected, `${label} SHA-256 does not match ${artifact.url}`);
    if (artifact.sizeBytes !== undefined && artifact.sizeBytes !== null) {
      assert(integer(artifact.sizeBytes, `${label}.sizeBytes`) === BigInt(bytes.length), `${label} size does not match ${artifact.url}`);
    }
    validated += 1;
  }
  return `${validated} local files`;
});

if (failures.length > 0) {
  console.error(`FAILED ${passes} checks passed; ${failures.length} failed`);
  process.exitCode = 1;
} else {
  console.log(`VERIFIED ${passes} checks passed; evidence is internally consistent`);
}
