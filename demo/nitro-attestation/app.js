const state = {
  evidence: null,
  source: "evidence.json",
  isExample: false,
};

const $ = (id) => document.getElementById(id);

const valueAt = (object, path, fallback = null) => {
  const value = path.split(".").reduce((current, key) => current?.[key], object);
  return value === undefined || value === null || value === "" ? fallback : value;
};

const text = (id, value, fallback = "—") => {
  const element = $(id);
  if (!element) return;
  const rendered = value === undefined || value === null || value === "" ? fallback : String(value);
  element.textContent = rendered;
  if (element.matches("code, .mono")) element.title = rendered;
};

const escapeHtml = (value) =>
  String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");

const safeHref = (value) => {
  if (!value || typeof value !== "string") return null;
  try {
    const url = new URL(value, window.location.href);
    if (["http:", "https:", "ipfs:"].includes(url.protocol)) return url.href;
    if (url.origin === window.location.origin) return url.href;
  } catch {
    return null;
  }
  return null;
};

const link = (id, href) => {
  const element = $(id);
  const safe = safeHref(href);
  if (!element) return;
  if (!safe) {
    element.classList.add("disabled");
    element.removeAttribute("target");
    element.removeAttribute("rel");
    if (element.getAttribute("href") === "#") element.removeAttribute("href");
    return;
  }
  element.href = safe;
  element.target = "_blank";
  element.rel = "noreferrer noopener";
  element.classList.remove("disabled");
};

const normalizeBlock = (value) => {
  if (value === undefined || value === null || value === "") return null;
  if (typeof value === "number") return Number.isFinite(value) ? BigInt(Math.trunc(value)) : null;
  try {
    return BigInt(String(value));
  } catch {
    return null;
  }
};

const formatNumber = (value) => {
  if (value === undefined || value === null || value === "") return null;
  try {
    return BigInt(String(value)).toLocaleString("en-US");
  } catch {
    return String(value);
  }
};

const formatBytes = (value) => {
  const bytes = Number(value);
  if (!Number.isFinite(bytes)) return null;
  if (bytes < 1024) return `${bytes.toLocaleString()} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
};

const formatTime = (value) => {
  if (!value) return null;
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return String(value);
  return new Intl.DateTimeFormat("en", {
    dateStyle: "medium",
    timeStyle: "medium",
    timeZone: "UTC",
  }).format(date) + " UTC";
};

const formatDuration = (startedAt, finishedAt, supplied) => {
  if (supplied) return String(supplied);
  const start = Date.parse(startedAt);
  const end = Date.parse(finishedAt);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return null;
  const total = Math.round((end - start) / 1000);
  if (total < 60) return `${total}s`;
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}m ${seconds}s`;
};

const statusPasses = (value) => {
  if (value === true || value === 1) return true;
  const normalized = String(value ?? "").toLowerCase();
  return ["0x1", "1", "success", "succeeded", "passed", "verified"].includes(normalized);
};

const statusFails = (value) => {
  if (value === false || value === 0) return true;
  const normalized = String(value ?? "").toLowerCase();
  return ["0x0", "0", "failure", "failed", "reverted", "invalid"].includes(normalized);
};

const equalHex = (left, right) => {
  if (!left || !right) return null;
  const normalize = (value) => String(value).toLowerCase().replace(/^0x/, "");
  return normalize(left) === normalize(right);
};

const evidenceBoolean = (value) => value === true ? true : value === false ? false : null;

const setStatusPill = (id, status, successLabel = "Verified", failureLabel = "Failed") => {
  const element = $(id);
  if (!element) return;
  element.classList.remove("success", "failure", "pending", "neutral");
  if (status === true) {
    element.classList.add("success");
    element.textContent = successLabel;
  } else if (status === false) {
    element.classList.add("failure");
    element.textContent = failureLabel;
  } else {
    element.classList.add("pending");
    element.textContent = "Pending";
  }
};

const computeChecks = (data) => {
  const receiptStatus = valueAt(data, "chain.transaction.status");
  const receiptSucceeded = receiptStatus === null ? null : statusPasses(receiptStatus) ? true : statusFails(receiptStatus) ? false : null;
  const targetMatches = equalHex(
    valueAt(data, "chain.transaction.to"),
    valueAt(data, "chain.precompile.address") ?? valueAt(data, "precompile.address"),
  );
  const transaction = receiptSucceeded === false || targetMatches === false
    ? false
    : receiptSucceeded === true && targetMatches === true
      ? true
      : null;

  const txBlock = normalizeBlock(valueAt(data, "chain.transaction.blockNumber"));
  const callBlock = normalizeBlock(valueAt(data, "chain.ethCall.blockNumber"));
  const decodedCall = valueAt(data, "chain.ethCall.decoded", {});
  const hasDecodedCall = Object.values(decodedCall).some((value) =>
    Array.isArray(value) ? value.length > 0 : value !== null && value !== undefined && value !== "",
  );
  const hasCallEvidence = Boolean(
    valueAt(data, "chain.ethCall.rawResult") ||
    valueAt(data, "chain.ethCall.resultSha256") ||
    hasDecodedCall,
  );
  const sameBlock = txBlock === null || callBlock === null || !hasCallEvidence ? null : txBlock === callBlock;

  const pcrs = Array.isArray(data.attestation?.pcrs) ? data.attestation.pcrs : [];
  const comparisons = pcrs.map((pcr) => ({ ...pcr, match: equalHex(pcr.expected, pcr.attested) }));
  const requiredPcrsPresent = [0, 1, 2].every((index) =>
    comparisons.some((pcr) => Number(pcr.index) === index),
  );
  const pcrsMatch = !requiredPcrsPresent || comparisons.some((pcr) => pcr.match === null)
    ? null
    : comparisons.every((pcr) => pcr.match);

  const documentHash = valueAt(data, "attestation.documentSha256");
  const certificateHash = valueAt(data, "attestation.certificateSha256");
  const document = documentHash && certificateHash ? true : null;

  const bindingEvidence = data.attestation?.bindings ?? {};
  const bindingChecks = {
    publicKey: evidenceBoolean(bindingEvidence.publicKeyMatchesRegistration),
    nonce: evidenceBoolean(bindingEvidence.nonceMatchesRegistration),
    userData: evidenceBoolean(bindingEvidence.userDataMatchesRegistrationChallenge),
  };
  const bindingValues = Object.values(bindingChecks);
  const bindings = bindingValues.includes(false)
    ? false
    : bindingValues.every((check) => check === true)
      ? true
      : null;

  return { transaction, sameBlock, pcrsMatch, document, bindings, bindingChecks, comparisons };
};

const renderVerdict = (data, checks) => {
  const reported = String(valueAt(data, "verification.status", "pending")).toLowerCase();
  const hasMismatch = [checks.transaction, checks.sameBlock, checks.pcrsMatch, checks.bindings].includes(false);
  const allChecksPass = [checks.transaction, checks.sameBlock, checks.pcrsMatch, checks.document, checks.bindings]
    .every((check) => check === true);
  const reportedSuccess = ["verified", "passed", "success", "succeeded"].includes(reported);
  const reportedFailure = ["failed", "failure", "invalid", "reverted"].includes(reported);
  const verdict = reportedFailure || hasMismatch ? "failed" : reportedSuccess && allChecksPass ? "verified" : "pending";

  const element = $("verdict");
  element.classList.remove("verified", "failed", "pending");
  element.classList.add(verdict);
  text("verdict-icon", verdict === "verified" ? "✓" : verdict === "failed" ? "×" : "…");
  text(
    "verdict-label",
    verdict === "verified" ? "Accepted by TIP-1090" : verdict === "failed" ? "Verification failed" : "Verification pending",
  );
  text("timeline-verdict", verdict === "verified" ? "Attestation verified" : verdict === "failed" ? "Verification failed" : "Verification pending");

  text("performed-at", formatTime(valueAt(data, "verification.performedAt")), "Timestamp pending");
  text(
    "verification-summary",
    valueAt(data, "verification.summary"),
    "The evidence bundle is incomplete. Supply captured deployment and chain data to render a final verdict.",
  );

  return verdict;
};

const renderCheck = (name, result, passText, failText, pendingText = "Pending") => {
  const item = document.querySelector(`[data-check="${name}"]`);
  const label = $(`check-${name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`)}`);
  if (!item || !label) return;
  item.classList.remove("passed", "failed");
  const mark = item.querySelector(".check-mark");
  if (result === true) {
    item.classList.add("passed");
    mark.textContent = "✓";
    label.textContent = passText;
  } else if (result === false) {
    item.classList.add("failed");
    mark.textContent = "×";
    label.textContent = failText;
  } else {
    mark.textContent = "·";
    label.textContent = pendingText;
  }
};

const renderOverview = (data, checks, verdict) => {
  const tx = data.chain?.transaction ?? {};
  text("metric-network", valueAt(data, "chain.name"));
  text("metric-chain-id", valueAt(data, "chain.chainId"));
  text("metric-block", formatNumber(tx.blockNumber));
  text("metric-gas", formatNumber(tx.gasUsed));
  text("hero-tx-hash", tx.hash, "Not supplied");
  text("receipt-id", tx.hash ? String(tx.hash).slice(0, 12) : null);
  link("tx-primary-link", tx.url || data.chain?.explorerUrl);

  renderCheck("transaction", checks.transaction, "Receipt + target valid", "Receipt or target failed");
  renderCheck("sameBlock", checks.sameBlock, "Block tags equal", "Block mismatch");
  renderCheck("pcrs", checks.pcrsMatch, "All values equal", "Measurement mismatch");
  renderCheck("document", checks.document, "Hashes captured", "Hashes missing", "Hashes pending");
  renderCheck("bindings", checks.bindings, "All fields bound", "Binding mismatch");

  const chip = $("dataset-chip");
  chip.classList.toggle("loaded", Boolean(data));
  const checkCount = valueAt(data, "verification.bundleVerifier.checksPassed");
  text(
    "dataset-label",
    state.isExample
      ? "Example schema"
      : verdict === "verified" && checkCount
        ? `${checkCount} bundle checks passed`
        : verdict === "verified"
          ? "Evidence loaded"
          : "Dataset loaded",
  );
};

const renderChain = (data, checks) => {
  const transaction = data.chain?.transaction ?? {};
  const ethCall = data.chain?.ethCall ?? {};
  const precompile = data.chain?.precompile ?? data.precompile ?? {};

  setStatusPill("receipt-status", checks.transaction, "Success", "Reverted");
  text("tx-hash", transaction.hash, "Not supplied");
  text("tx-block", formatNumber(transaction.blockNumber));
  text("tx-block-hash", transaction.blockHash, "Not supplied");
  text("tx-from", transaction.from, "Not supplied");
  text("tx-to", transaction.to || precompile.address, "Not supplied");
  text("tx-gas", formatNumber(transaction.gasUsed));
  text("tx-input-hash", transaction.inputSha256, "Not supplied");
  link("tx-link", transaction.url);

  const callHasResult = Boolean(ethCall.rawResult || ethCall.resultSha256 || ethCall.decoded);
  setStatusPill("call-status", callHasResult ? true : null, "Decoded", "Failed");
  text("call-block", formatNumber(ethCall.blockNumber));
  text("call-selector", ethCall.functionSelector || precompile.functionSelector, "Not supplied");
  text("call-output-hash", ethCall.resultSha256, "Not supplied");
  text("raw-call-output", ethCall.rawResult, "Not supplied");

  const blockLock = $("call-block-lock");
  blockLock.classList.remove("match", "mismatch");
  if (checks.sameBlock === true) {
    blockLock.classList.add("match");
    text("call-block-comparison", `Matches receipt block ${formatNumber(transaction.blockNumber)}`);
  } else if (checks.sameBlock === false) {
    blockLock.classList.add("mismatch");
    text("call-block-comparison", "Block mismatch detected");
  } else {
    text("call-block-comparison", "Block equality pending");
  }

  text("precompile-address", precompile.address, "Not supplied");
  text("precompile-signature", precompile.functionSignature, "Not supplied");
};

const renderAttestation = (data, checks) => {
  const attestation = data.attestation ?? {};
  text("module-id-chip", attestation.moduleId ? `Module · ${attestation.moduleId}` : null, "Module pending");
  text("claim-module-id", attestation.moduleId);
  text("claim-timestamp-iso", formatTime(attestation.timestampIso ?? attestation.timestamp), "—");
  text("claim-timestamp", attestation.timestamp === null || attestation.timestamp === undefined ? null : `${attestation.timestamp} ms`);
  text("claim-digest", attestation.digest);
  text("claim-certificate-hash", attestation.certificateSha256);
  text("claim-document-hash", attestation.documentSha256);
  text("claim-document-size", formatBytes(attestation.sizeBytes));
  text("claim-public-key", attestation.publicKey, "Not supplied");
  text("claim-user-data", attestation.userData, "Not supplied");
  text("claim-nonce", attestation.nonce, "Not supplied");

  setStatusPill("binding-summary", checks.bindings, "All bound", "Mismatch");
  const bindingRows = [
    ["public-key", "binding-public-key", checks.bindingChecks.publicKey],
    ["nonce", "binding-nonce", checks.bindingChecks.nonce],
    ["user-data", "binding-user-data", checks.bindingChecks.userData],
  ];
  for (const [rowName, id, result] of bindingRows) {
    const row = document.querySelector(`[data-binding-row="${rowName}"]`);
    row.classList.remove("passed", "failed");
    if (result === true) {
      row.classList.add("passed");
      text(id, "Match");
    } else if (result === false) {
      row.classList.add("failed");
      text(id, "Mismatch");
    } else {
      text(id, "Pending");
    }
  }

  const grid = $("pcr-grid");
  grid.replaceChildren();
  if (!checks.comparisons.length) {
    const placeholder = document.createElement("article");
    placeholder.className = "pcr-card placeholder-card";
    placeholder.textContent = "PCR data pending";
    grid.append(placeholder);
    return;
  }

  for (const pcr of checks.comparisons) {
    const matchClass = pcr.match === true ? "match" : pcr.match === false ? "mismatch" : "";
    const label = pcr.match === true ? "Exact match" : pcr.match === false ? "Mismatch" : "Pending";
    const pillClass = pcr.match === true ? "success" : pcr.match === false ? "failure" : "pending";
    const card = document.createElement("article");
    card.className = `pcr-card ${matchClass}`.trim();
    card.innerHTML = `
      <div class="pcr-card-head">
        <strong>PCR${escapeHtml(pcr.index ?? "?")}</strong>
        <span class="status-pill ${pillClass}">${label}</span>
      </div>
      <div class="pcr-comparison">
        <div class="pcr-value">
          <span>Expected · EIF build</span>
          <code title="${escapeHtml(pcr.expected ?? "Not supplied")}">${escapeHtml(pcr.expected ?? "Not supplied")}</code>
        </div>
        <div class="pcr-equals">${pcr.match === true ? "equals" : pcr.match === false ? "differs" : "compare"}</div>
        <div class="pcr-value">
          <span>Attested · AWS NSM</span>
          <code title="${escapeHtml(pcr.attested ?? "Not supplied")}">${escapeHtml(pcr.attested ?? "Not supplied")}</code>
        </div>
      </div>`;
    grid.append(card);
  }
};

const renderRuntime = (data) => {
  const aws = data.aws ?? {};
  const enclave = aws.enclave ?? {};
  const devnet = data.devnet ?? data.provenance?.devnet ?? {};

  text("aws-account", aws.accountId);
  text("aws-region", aws.region);
  text("aws-az", aws.availabilityZone);
  text("aws-instance-id", aws.instanceId);
  text("aws-instance-type", aws.instanceType);
  text("aws-ami", aws.amiId);
  text("aws-state", aws.state);

  text("enclave-id", enclave.id);
  text("enclave-state", enclave.state);
  text("enclave-cid", enclave.cid);
  text("enclave-cpus", enclave.cpuCount);
  text("enclave-memory", enclave.memoryMiB === null || enclave.memoryMiB === undefined ? null : `${formatNumber(enclave.memoryMiB)} MiB`);
  text("enclave-eif-name", enclave.eifName);
  text("enclave-eif-hash", enclave.eifSha384);

  text("devnet-name", devnet.name);
  text("devnet-namespace", devnet.namespace);
  text("devnet-rpc", devnet.rpcUrl);
  text("devnet-chain-id", devnet.chainId ?? data.chain?.chainId);
  text("devnet-client", devnet.clientVersion);
  text("devnet-image-digest", devnet.imageDigest);
  text("devnet-head", formatNumber(devnet.headBlockAtCapture));
};

const renderProvenance = (data, verdict) => {
  const provenance = data.provenance ?? {};
  const tempo = provenance.tempo ?? {};
  const zones = provenance.zones ?? {};
  const workflow = provenance.workflow ?? {};
  const transaction = data.chain?.transaction ?? {};

  const prLabel = tempo.prNumber ? `PR #${tempo.prNumber}` : "PR pending";
  text("tempo-pr-title", tempo.prTitle ? `${prLabel} · ${tempo.prTitle}` : prLabel);
  text("tempo-commit", tempo.commit, "Commit not supplied");
  text("tempo-branch", tempo.branch, "Branch pending");
  text("tempo-image", tempo.imageDigest || tempo.image, "Image pending");
  link("tempo-pr-link", tempo.prUrl);

  text("zones-repository", zones.repository, "Repository pending");
  text("zones-commit", zones.commit, "Commit not supplied");
  text("zones-branch", zones.branch, "Branch pending");
  text("zones-image", zones.imageDigest || zones.image, "Image pending");
  link("zones-commit-link", zones.commitUrl || zones.repositoryUrl);

  text("workflow-name", workflow.name, "Workflow pending");
  text("workflow-namespace", workflow.namespace, "Namespace not supplied");
  text("workflow-status", workflow.status, "Status pending");
  text("workflow-duration", formatDuration(workflow.startedAt, workflow.finishedAt, workflow.duration), "Duration pending");
  link("workflow-link", workflow.url);

  text("timeline-verdict", verdict === "verified" ? "Attestation verified" : verdict === "failed" ? "Verification failed" : "Verification pending");
  text("timeline-tx", transaction.hash, "Transaction not supplied");
  text("timeline-block", transaction.blockNumber === null || transaction.blockNumber === undefined ? null : `Block ${formatNumber(transaction.blockNumber)}`, "Block pending");
  text("timeline-time", formatTime(valueAt(data, "verification.performedAt")), "Time pending");
};

const renderArtifacts = (data) => {
  const artifacts = Array.isArray(data.artifacts) ? data.artifacts : [];
  const grid = $("artifact-grid");
  grid.replaceChildren();
  if (!artifacts.length) {
    const placeholder = document.createElement("article");
    placeholder.className = "artifact-card placeholder-card";
    placeholder.textContent = "Artifact manifest pending";
    grid.append(placeholder);
    return;
  }

  for (const artifact of artifacts) {
    const card = document.createElement("article");
    card.className = "artifact-card";
    const href = safeHref(artifact.url);
    const label = escapeHtml(artifact.label ?? "Unnamed artifact");
    card.innerHTML = `
      <div class="artifact-kind">
        <span>${escapeHtml(artifact.kind ?? "artifact")}</span>
        <span>${escapeHtml(artifact.sizeBytes ? formatBytes(artifact.sizeBytes) : "")}</span>
      </div>
      <h3>${href ? `<a href="${escapeHtml(href)}" target="_blank" rel="noreferrer noopener">${label} ↗</a>` : label}</h3>
      <p>${escapeHtml(artifact.description ?? "Captured verification evidence")}</p>
      <code title="${escapeHtml(artifact.sha256 ?? "Checksum not supplied")}">SHA-256 · ${escapeHtml(artifact.sha256 ?? "Not supplied")}</code>`;
    grid.append(card);
  }
};

const renderScope = (data) => {
  const limitations = Array.isArray(data.limitations) ? data.limitations : [];
  const list = $("scope-limitations");
  list.replaceChildren();
  if (!limitations.length) {
    const item = document.createElement("li");
    item.textContent = "Scope limits were not supplied with this dataset.";
    list.append(item);
    return;
  }
  for (const limitation of limitations) {
    const item = document.createElement("li");
    item.textContent = String(limitation);
    list.append(item);
  }
};

const syntaxHighlight = (data) => {
  const json = escapeHtml(JSON.stringify(data, null, 2));
  return json.replace(
    /(&quot;(?:\\u[a-fA-F0-9]{4}|\\[^u]|[^\\&])*&quot;\s*:?)|\b(true|false)\b|\b(null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g,
    (match) => {
      if (match.startsWith("&quot;")) return `<span class="${match.endsWith(":") ? "json-key" : "json-string"}">${match}</span>`;
      if (match === "true" || match === "false") return `<span class="json-boolean">${match}</span>`;
      if (match === "null") return `<span class="json-null">${match}</span>`;
      return `<span class="json-number">${match}</span>`;
    },
  );
};

const renderRaw = (data) => {
  text("raw-dataset-name", state.source);
  $("raw-json").innerHTML = `<code>${syntaxHighlight(data)}</code>`;
  text("footer-generated", formatTime(valueAt(data, "verification.performedAt")), "Evidence timestamp pending");
};

const render = (data) => {
  const checks = computeChecks(data);
  const verdict = renderVerdict(data, checks);
  renderOverview(data, checks, verdict);
  renderChain(data, checks);
  renderAttestation(data, checks);
  renderRuntime(data);
  renderProvenance(data, verdict);
  renderScope(data);
  renderArtifacts(data);
  renderRaw(data);
};

const showBanner = (title, message, type = "warning") => {
  const banner = $("data-banner");
  banner.classList.remove("hidden", "error");
  if (type === "error") banner.classList.add("error");
  text("data-banner-title", title);
  text("data-banner-message", message, "");
};

const loadJson = async (path) => {
  const response = await fetch(path, { cache: "no-store" });
  if (!response.ok) throw new Error(`${path} returned HTTP ${response.status}`);
  return response.json();
};

const loadEvidence = async () => {
  try {
    state.evidence = await loadJson("./evidence.json");
    state.source = "evidence.json";
  } catch (evidenceError) {
    try {
      state.evidence = await loadJson("./evidence.example.json");
      state.source = "evidence.example.json";
      state.isExample = true;
      showBanner(
        "Example schema",
        "evidence.json was not found. The page is showing empty placeholders from evidence.example.json; no live result is being claimed.",
      );
    } catch (exampleError) {
      showBanner(
        "Evidence unavailable",
        `${evidenceError.message}. ${exampleError.message}. Serve this directory over HTTP and provide evidence.json.`,
        "error",
      );
      state.evidence = {
        schemaVersion: "1.0",
        verification: { status: "pending", summary: "No evidence bundle could be loaded." },
      };
      state.source = "unavailable";
    }
  }
  render(state.evidence);
};

const copyText = async (value, button) => {
  if (!value || value === "Not supplied" || value === "—") return;
  try {
    await navigator.clipboard.writeText(value);
    const previous = button.textContent;
    button.textContent = "Copied";
    setTimeout(() => { button.textContent = previous; }, 1200);
  } catch {
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(button.previousElementSibling ?? button);
    selection.removeAllRanges();
    selection.addRange(range);
  }
};

document.addEventListener("click", (event) => {
  const copyButton = event.target.closest("[data-copy-source]");
  if (copyButton) {
    const source = $(copyButton.dataset.copySource);
    copyText(source?.textContent, copyButton);
  }
});

$("show-call-output").addEventListener("click", () => {
  const output = $("raw-call-output");
  output.classList.toggle("hidden");
  $("show-call-output").lastElementChild.textContent = output.classList.contains("hidden") ? "↓" : "↑";
});

$("copy-json").addEventListener("click", (event) => {
  if (state.evidence) copyText(JSON.stringify(state.evidence, null, 2), event.currentTarget);
});

$("download-json").addEventListener("click", () => {
  if (!state.evidence) return;
  const blob = new Blob([JSON.stringify(state.evidence, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = state.source.endsWith(".json") ? state.source : "evidence.json";
  anchor.click();
  URL.revokeObjectURL(url);
});

loadEvidence();
