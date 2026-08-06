const scenarioOrder = ["deposit", "earn_deposit", "earn_redeem", "withdraw"];
const fallbackScenarioNames = {
  deposit: "Onramp to Zone",
  earn_deposit: "Earn Vault Deposit",
  earn_redeem: "Earn Vault Redeem",
  withdraw: "Offramp to Tempo",
};
const scenarioStepLabels = {
  deposit: [
    ["onramp.encryption", "Encrypt deposit"],
    ["onramp.enqueued", "Deposit accepted in Portal"],
    ["onramp.zone_deposit.processed", "Funds available in Zone"],
  ],
  earn_deposit: [
    ["earn_deposit.encryption", "Encrypt withdrawal"],
    ["earn_deposit.request_result", "Withdrawal confirmed in Zone"],
    ["earn_deposit.l1_processed_locator", "Process on Tempo + deposit into Earn vault"],
    ["earn_deposit.zone_return.processed", "Vault shares into Zone"],
  ],
  earn_redeem: [
    ["earn_redeem.encryption", "Encrypt withdrawal"],
    ["earn_redeem.request_result", "Withdrawal confirmed in Zone"],
    ["earn_redeem.l1_processed_locator", "Process on Tempo + redeem from Earn vault"],
    ["earn_redeem.zone_return.processed", "Redeemed funds into Zone"],
  ],
  withdraw: [
    ["offramp", "Request Zone withdrawal"],
    ["offramp_result", "Zone accepts withdrawal"],
    ["offramp_processed", "Funds arrive on L1"],
  ],
};
const activeStates = new Set(["queued", "running", "processing", "cancelling"]);

const elements = {
  connectionPill: document.querySelector("#connection-pill"),
  branchLabel: document.querySelector("#branch-label"),
  configCount: document.querySelector("#config-count"),
  launchNote: document.querySelector("#launch-note"),
  toast: document.querySelector("#toast"),
};

let state = null;
let selectedScenario = "deposit";
let pollTimer = null;
let toastTimer = null;
let tickTimer = null;
let liveTick = null; // { scenario, startedAt, total, completed }

function scenarioName(id) {
  const definition = state?.server?.scenarios?.find((scenario) => scenario.id === id);
  return definition?.shortTitle || fallbackScenarioNames[id] || id;
}

function formatDuration(milliseconds) {
  const value = Number(milliseconds || 0);
  if (value <= 0) return "—";
  if (value < 1) return `${value.toFixed(2)} ms`;
  if (value < 1000) return `${Math.round(value)} ms`;
  return `${(value / 1000).toFixed(value < 10000 ? 2 : 1)} s`;
}

function formatUsd(value) {
  const amount = Number(value || 0);
  if (amount >= 0.01) return `$${amount.toFixed(2)}`;
  if (amount <= 0) return "$0.000000";
  // Sub-cent fees: show six decimal places; fall back to two significant
  // figures when the value is smaller than six decimals can express.
  let text = amount.toFixed(6);
  if (Number(text) === 0) {
    const trimmed = amount.toFixed(12).replace(/0+$/, "");
    const match = trimmed.match(/^0\.(0*)(\d{1,2})/);
    if (match) text = `0.${match[1]}${match[2]}`;
  }
  return `$${text}`;
}

function formatGas(value) {
  const amount = Number(value || 0);
  if (!amount) return "0";
  if (amount >= 1e6) return `${(amount / 1e6).toFixed(2)}M`;
  if (amount >= 1e4) return `${Math.round(amount / 1e3)}k`;
  return Math.round(amount).toLocaleString();
}

function formatElapsed(ms) {
  const seconds = Math.max(0, Number(ms || 0)) / 1000;
  return seconds < 10 ? `${seconds.toFixed(1)}s` : `${Math.round(seconds)}s`;
}

function createElement(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function showToast(message) {
  clearTimeout(toastTimer);
  elements.toast.textContent = message;
  elements.toast.hidden = false;
  toastTimer = setTimeout(() => { elements.toast.hidden = true; }, 6500);
}

function renderConnection(server = {}) {
  const ready = Boolean(server.localReady);
  elements.connectionPill.dataset.ready = String(ready);
  elements.connectionPill.querySelector("span").textContent = ready
    ? "Local runner ready"
    : "Local tools missing";
  if (elements.branchLabel) elements.branchLabel.textContent = server.branch || "Unknown branch";
  elements.launchNote.textContent = ready
    ? (activeStates.has(state?.status) ? state.message : "Local Tempo + private Zone")
    : `Missing: ${(server.missingTools || []).join(", ")}`;
  return ready;
}

function setMetric(card, key, value, label) {
  const strong = card.querySelector(`[data-metric="${key}"]`);
  if (!strong) return;
  strong.textContent = value;
  const span = strong.previousElementSibling;
  if (label && span && span.tagName === "SPAN") span.textContent = label;
}

function renderCard(id, canRun, anyRunning) {
  const card = document.querySelector(`[data-scenario="${id}"]`);
  const entry = state.scenarioResults?.[id];
  const isCurrent = state.run?.scenario === id;
  const running = isCurrent && activeStates.has(state.status);
  const failed = isCurrent && ["failed", "interrupted"].includes(state.status);
  const result = entry?.result;
  const summary = result?.summary;

  card.classList.toggle("is-selected", selectedScenario === id);
  card.classList.toggle("is-running", running);
  card.classList.toggle("is-complete", Boolean(result) && !running);
  card.classList.toggle("is-failed", failed);

  const status = card.querySelector(".scenario-status");
  status.textContent = running ? "Running" : failed ? "Failed" : result ? "Complete" : "Ready";

  if (running) {
    renderProgress(card, state.progress, state.run);
  } else {
    clearProgress(card);
    // A single transaction has no distribution — drop p99/p50/avg wording and
    // show just the measured value (+ its end-to-end journey for context).
    const single = Boolean(summary) && Number(summary.completed) <= 1;
    const latencySub = card.querySelector('[data-metric="latency-sub"]');
    // All N txs land in one block, so p50==p99 (sub-ms spread) — show one
    // honest latency number instead of a redundant percentile split.
    setMetric(card, "p99", summary ? formatDuration(summary.p99Ms) : "—", "Latency");
    if (latencySub) latencySub.textContent = "";
    setMetric(card, "cost", summary ? formatUsd(summary.meanJourneyCostUsd) : "—",
      single ? "Fee (USD)" : "Avg fee (USD)");
    setMetric(card, "gas", summary ? formatGas(summary.meanJourneyGas) : "—",
      single ? "Gas used" : "Avg gas used");
  }
  renderCardSteps(card, id, result, running);

  const button = card.querySelector(".go-button");
  button.disabled = anyRunning || !canRun;
  button.querySelector("span").textContent = running ? "RUNNING" : result ? "AGAIN" : "GO";
}

function renderProgress(card, progress, run) {
  const body = card.querySelector(".lane-body");
  const metrics = card.querySelector(".card-metrics");
  if (metrics) metrics.style.display = "none";

  let node = body.querySelector(".card-progress");
  if (!node) {
    node = createElement("div", "card-progress");
    node.innerHTML =
      '<div class="progress-count"><b class="js-done">0</b><em class="js-total">/ 0</em><span>completed</span></div>' +
      '<div class="progress-bar"><i></i></div>' +
      '<div class="progress-sub">' +
      '<span><b class="js-inflight">0</b> in flight</span>' +
      '<span><b class="js-elapsed">0.0s</b> elapsed</span>' +
      '<span><b class="js-tps">—</b> tx/s</span></div>';
    body.append(node);
  }

  const total = Number(progress?.total || run?.config?.count || 0);
  const completed = Number(progress?.completed || 0);
  const inFlight = Number(progress?.inFlight || 0);
  const elapsedMs = run?.startedAt ? Date.now() - Date.parse(run.startedAt) : Number(progress?.elapsedMs || 0);

  node.querySelector(".js-done").textContent = completed.toLocaleString();
  node.querySelector(".js-total").textContent = `/ ${total.toLocaleString()}`;
  node.querySelector(".js-inflight").textContent = inFlight.toLocaleString();
  node.querySelector(".js-elapsed").textContent = formatElapsed(elapsedMs);
  node.querySelector(".js-tps").textContent =
    completed > 0 && elapsedMs > 200 ? Math.max(1, Math.round(completed / (elapsedMs / 1000))).toLocaleString() : "—";

  const bar = node.querySelector(".progress-bar");
  const fill = bar.querySelector("i");
  const indeterminate = !(total > 0 && completed > 0);
  bar.classList.toggle("indeterminate", indeterminate);
  fill.style.width = indeterminate ? "" : `${Math.min(100, (completed / total) * 100)}%`;

  const hasLiveCounts = completed > 0 || inFlight > 0;
  node.querySelectorAll(".progress-sub span").forEach((sp, i) => {
    if (i !== 1) sp.style.display = hasLiveCounts ? "" : "none";
  });
  liveTick = { scenario: card.dataset.scenario, startedAt: run?.startedAt, total, completed };
}

function clearProgress(card) {
  const node = card.querySelector(".card-progress");
  if (node) node.remove();
  const metrics = card.querySelector(".card-metrics");
  if (metrics) metrics.style.display = "";
}

function renderCardSteps(card, id, result, running) {
  const list = card.querySelector("[data-card-steps]");
  const measured = new Map(
    running ? [] : (result?.steps || []).map((step) => [step.name, step]),
  );
  list.replaceChildren();
  for (const [name, label] of scenarioStepLabels[id]) {
    const step = measured.get(name);
    const item = createElement("li");
    item.dataset.status = step ? "completed" : "queued";
    item.title = label;
    item.append(createElement("i"), createElement("strong", "", step ? formatDuration(step.p99Ms) : "—"));
    list.append(item);
  }
}

function renderScenarioConnectors(anyRunning) {
  document.querySelectorAll(".connector").forEach((connector, index) => {
    const previous = scenarioOrder[index];
    const next = scenarioOrder[index + 1];
    connector.classList.toggle("is-lit", Boolean(state.scenarioResults?.[previous]));
    connector.classList.toggle("is-active", anyRunning && state.run?.scenario === next);
  });
}

function tickElapsed() {
  if (!liveTick || !liveTick.startedAt) return;
  const card = document.querySelector(`[data-scenario="${liveTick.scenario}"]`);
  const node = card?.querySelector(".card-progress");
  if (!node) return;
  const elapsedMs = Date.now() - Date.parse(liveTick.startedAt);
  node.querySelector(".js-elapsed").textContent = formatElapsed(elapsedMs);
  node.querySelector(".js-tps").textContent =
    liveTick.completed > 0 && elapsedMs > 200
      ? Math.max(1, Math.round(liveTick.completed / (elapsedMs / 1000))).toLocaleString()
      : "—";
}

function render(nextState) {
  const priorStatus = state?.status;
  state = nextState;
  const anyRunning = activeStates.has(state.status);
  const canRun = renderConnection(state.server);
  if (
    state.run?.scenario
    && (anyRunning || (activeStates.has(priorStatus) && state.status === "completed"))
  ) selectedScenario = state.run.scenario;
  if (!anyRunning) liveTick = null;
  for (const id of scenarioOrder) renderCard(id, canRun, anyRunning);
  renderScenarioConnectors(anyRunning);

  if (anyRunning && !tickTimer) tickTimer = setInterval(tickElapsed, 100);
  if (!anyRunning && tickTimer) { clearInterval(tickTimer); tickTimer = null; }

  if (priorStatus && activeStates.has(priorStatus) && state.status === "completed") {
    document.querySelector(`[data-scenario="${selectedScenario}"]`)?.focus({ preventScroll: true });
  }
  if (priorStatus && activeStates.has(priorStatus) && state.status === "failed") {
    showToast(state.error || "Local benchmark failed");
  }
}

async function request(path, options = {}) {
  const response = await fetch(path, { headers: { "Content-Type": "application/json" }, ...options });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(payload.error || `Request failed (${response.status})`);
  return payload;
}

async function poll() {
  clearTimeout(pollTimer);
  try {
    render(await request("/api/state"));
  } catch (error) {
    elements.connectionPill.querySelector("span").textContent = "Server disconnected";
    showToast(error.message);
  } finally {
    pollTimer = setTimeout(poll, activeStates.has(state?.status) ? 250 : 4000);
  }
}

async function runScenario(id) {
  selectedScenario = id;
  try {
    const count = Number(elements.configCount.value || 50);
    const nextState = await request("/api/runs", {
      method: "POST",
      body: JSON.stringify({ scenario: id, count }),
    });
    render(nextState);
    clearTimeout(pollTimer);
    pollTimer = setTimeout(poll, 300);
  } catch (error) {
    showToast(error.message);
  }
}

document.querySelectorAll("[data-run]").forEach((button) => {
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    runScenario(button.dataset.run);
  });
});

document.querySelectorAll(".scenario-card").forEach((card) => {
  const select = () => { selectedScenario = card.dataset.scenario; if (state) render(state); };
  card.addEventListener("click", select);
  card.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") { event.preventDefault(); select(); }
  });
});

poll();
