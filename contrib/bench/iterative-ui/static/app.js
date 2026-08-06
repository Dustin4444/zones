const scenarioOrder = ["deposit", "earn_deposit", "earn_redeem", "withdraw"];
const fallbackScenarioNames = {
  deposit: "Onramp to Zone",
  earn_deposit: "Private Earn Vault Deposit",
  earn_redeem: "Private Earn Vault Redeem",
  withdraw: "Offramp back to Tempo",
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
    ["earn_deposit.l1_processed_locator", "Process withdrawal on Tempo + deposit into Earn vault"],
    ["earn_deposit.zone_return.processed", "Deposit vault shares into Zone"],
  ],
  earn_redeem: [
    ["earn_redeem.encryption", "Encrypt withdrawal"],
    ["earn_redeem.request_result", "Withdrawal confirmed in Zone"],
    ["earn_redeem.l1_processed_locator", "Process withdrawal on Tempo + redeem from Earn vault"],
    ["earn_redeem.zone_return.processed", "Deposit redeemed funds into Zone"],
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
  configRate: document.querySelector("#config-rate"),
  configConcurrency: document.querySelector("#config-concurrency"),
  settingsToggle: document.querySelector("#settings-toggle"),
  settingsForm: document.querySelector("#settings-form"),
  launchNote: document.querySelector("#launch-note"),
  historySection: document.querySelector("#history-section"),
  historyList: document.querySelector("#history-list"),
  toast: document.querySelector("#toast"),
};

let state = null;
let selectedScenario = "deposit";
let pollTimer = null;
let toastTimer = null;

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
  return `$${amount.toFixed(8)}`;
}

function formatGas(value) {
  const amount = Number(value || 0);
  return amount ? Math.round(amount).toLocaleString() : "0";
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
  const label = elements.connectionPill.querySelector("span");
  label.textContent = ready ? "Local runner ready" : "Local tools missing";
  elements.branchLabel.textContent = server.branch || "Unknown branch";
  elements.launchNote.textContent = ready
    ? (activeStates.has(state?.status) ? state.message : "Local Tempo + private Zone")
    : `Missing: ${(server.missingTools || []).join(", ")}`;
  return ready;
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
  if (running) {
    status.textContent = "Running";
  } else if (failed) {
    status.textContent = "Failed";
  } else if (result) {
    status.textContent = "Complete";
  } else {
    status.textContent = "Ready";
  }

  card.querySelector('[data-metric="p99"]').textContent = summary ? formatDuration(summary.p99Ms) : "—";
  card.querySelector('[data-metric="cost"]').textContent = summary ? formatUsd(summary.meanJourneyCostUsd) : "—";
  card.querySelector('[data-metric="gas"]').textContent = summary ? formatGas(summary.meanJourneyGas) : "—";
  renderCardSteps(card, id, result, running);

  const button = card.querySelector(".go-button");
  button.disabled = anyRunning || !canRun;
  button.querySelector("span").textContent = running ? "RUNNING" : result ? "GO AGAIN" : "GO";
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
    const value = step ? formatDuration(step.p99Ms) : "—";
    item.append(createElement("i"), createElement("span", "", label), createElement("strong", "", value));
    list.append(item);
  }
}

function renderScenarioConnectors(anyRunning) {
  document.querySelectorAll(".connector").forEach((connector, index) => {
    const previous = scenarioOrder[index];
    const next = scenarioOrder[index + 1];
    connector.classList.toggle("is-lit", Boolean(state.scenarioResults?.[previous]));
    connector.classList.toggle(
      "is-active",
      anyRunning && state.run?.scenario === next,
    );
  });
}

function renderHistory(history = []) {
  elements.historySection.hidden = history.length === 0;
  elements.historyList.replaceChildren();
  for (const entry of history) {
    const summary = entry.summary || {};
    const item = createElement("article", "history-item");
    const identity = createElement("div");
    identity.append(
      createElement("strong", "", scenarioName(entry.scenario)),
      createElement("code", "", entry.startedAt ? new Date(entry.startedAt).toLocaleString() : ""),
    );
    const latency = createElement("div");
    latency.append(createElement("strong", "", formatDuration(summary.p99Ms)), document.createTextNode("p99 latency"));
    const cost = createElement("div");
    cost.append(createElement("strong", "", formatUsd(summary.meanJourneyCostUsd)), document.createTextNode("average fee"));
    item.append(identity, latency, cost);
    if (entry.url) {
      const link = createElement("a", "", "Open ↗");
      link.href = entry.url;
      link.target = "_blank";
      link.rel = "noreferrer";
      item.append(link);
    }
    elements.historyList.append(item);
  }
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
  for (const id of scenarioOrder) renderCard(id, canRun, anyRunning);
  renderScenarioConnectors(anyRunning);
  renderHistory(state.history || []);
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
    const count = Number(elements.configCount.value || 100);
    const concurrency = Number(elements.configConcurrency.value || 12);
    const nextState = await request("/api/runs", {
      method: "POST",
      body: JSON.stringify({
        scenario: id,
        count,
        rate: elements.configRate.value,
        concurrency,
      }),
    });
    render(nextState);
    clearTimeout(pollTimer);
    pollTimer = setTimeout(poll, 400);
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

elements.settingsToggle.addEventListener("click", () => {
  const expanded = elements.settingsToggle.getAttribute("aria-expanded") === "true";
  elements.settingsToggle.setAttribute("aria-expanded", String(!expanded));
  elements.settingsForm.hidden = expanded;
  elements.settingsToggle.textContent = expanded ? "Edit" : "Done";
});

poll();
