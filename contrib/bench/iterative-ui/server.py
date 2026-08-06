#!/usr/bin/env python3
"""Local control room for the production-shaped neobank benchmark.

The browser never receives GitHub credentials. This server uses the authenticated
`gh` CLI to dispatch the existing benchmark workflow, follows its real job
steps, downloads the txgen artifact, and converts it into presentation-friendly
latency, throughput, gas, and fee data.
"""

from __future__ import annotations

import argparse
import copy
import json
import os
import re
import shutil
import subprocess
import threading
import time
import webbrowser
from datetime import datetime, timedelta, timezone
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlparse


REPOSITORY = "tempoxyz/zones"
WORKFLOW = "zones-benchmark.yml"
WORKFLOW_URL = f"https://github.com/{REPOSITORY}/actions/workflows/{WORKFLOW}"
STATE_VERSION = 3
BRANCH_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/-]*$")
PHASES = (
    {
        "id": "deposit",
        "title": "Deposit into the Zone",
        "eyebrow": "L1 → private Zone",
        "prefixes": ("onramp.",),
        "critical": (
            "onramp.encryption",
            "onramp.submission",
            "onramp.enqueued",
            "onramp.zone_deposit.processed",
        ),
        "terminal": "onramp.zone_deposit.processed",
    },
    {
        "id": "earn_deposit",
        "title": "Put funds into Earn",
        "eyebrow": "Zone → L1 Earn → Zone",
        "prefixes": ("earn_deposit.",),
        "critical": (
            "earn_deposit.encryption",
            "earn_deposit.request",
            "earn_deposit.l1_processed_locator",
            "earn_deposit.l1_result",
            "earn_deposit.zone_return.processed",
        ),
        "terminal": "earn_deposit.zone_return.processed",
    },
    {
        "id": "earn_redeem",
        "title": "Redeem vault shares",
        "eyebrow": "Zone → L1 redeem → Zone",
        "prefixes": ("earn_redeem.",),
        "critical": (
            "earn_redeem.encryption",
            "earn_redeem.request",
            "earn_redeem.l1_processed_locator",
            "earn_redeem.l1_result",
            "earn_redeem.zone_return.processed",
        ),
        "terminal": "earn_redeem.zone_return.processed",
    },
    {
        "id": "withdraw",
        "title": "Withdraw back to L1",
        "eyebrow": "Private Zone → L1 wallet",
        "prefixes": ("offramp", "l1_before_offramp"),
        "critical": ("l1_before_offramp", "offramp", "offramp_processed"),
        "terminal": "offramp_processed",
    },
)

SCENARIOS = {
    "deposit": {
        "title": "Deposit into the Zone",
        "shortTitle": "Onramp to Zone",
        "description": "Move dLUSD from the customer's L1 wallet into the private Zone.",
        "route": "L1 wallet → private Zone",
        "preset": "encrypted-deposit",
        "phase": "deposit",
    },
    "earn_deposit": {
        "title": "Deposit into Earn",
        "shortTitle": "Private Earn Vault Deposit",
        "description": "Send Zone funds into Earn on L1 and return the vault shares.",
        "route": "Zone ↔ Earn Vault",
        "preset": "earn-deposit",
        "phase": "earn_deposit",
    },
    "earn_redeem": {
        "title": "Redeem from Earn",
        "shortTitle": "Private Earn Vault Redeem",
        "description": "Redeem the vault shares on L1 and return dLUSD to the Zone.",
        "route": "Earn Vault ↔ Zone",
        "preset": "private-withdrawal",
        "phase": "earn_redeem",
    },
    "withdraw": {
        "title": "Withdraw to L1",
        "shortTitle": "Offramp back to Tempo",
        "description": "Move dLUSD out of the private Zone and back to the customer's L1 wallet.",
        "route": "Private Zone → L1 wallet",
        "preset": "zone-withdrawal",
        "phase": "withdraw",
    },
}

FRIENDLY_STEP_NAMES = {
    "onramp.encryption": "Encrypt deposit",
    "onramp.submission": "Submit deposit on L1",
    "onramp.enqueued": "Deposit accepted in Portal",
    "onramp.zone_deposit.processed": "Funds available in the Zone",
    "earn_deposit.encryption": "Encrypt withdrawal",
    "earn_deposit.request": "Submit withdrawal tx",
    "earn_deposit.request_result": "Withdrawal confirmed in Zone",
    "earn_deposit.l1_processed_locator": "Process withdrawal on Tempo + deposit into Earn vault",
    "earn_deposit.l1_result": "Read Earn deposit event",
    "earn_deposit.zone_return.processed": "Deposit vault shares into Zone",
    "earn_redeem.encryption": "Encrypt withdrawal",
    "earn_redeem.request": "Submit withdrawal tx",
    "earn_redeem.request_result": "Withdrawal confirmed in Zone",
    "earn_redeem.l1_processed_locator": "Process withdrawal on Tempo + redeem from Earn vault",
    "earn_redeem.l1_result": "Read Earn redemption event",
    "earn_redeem.zone_return.processed": "Deposit redeemed funds into Zone",
    "offramp": "Request withdrawal to L1",
    "offramp_result": "Withdrawal accepted by Zone",
    "offramp_processed": "Funds received on L1",
}

DISPLAY_JOB_STEPS = {
    "Resolve configuration": ("request", "Validate benchmark request"),
    "Build real L1 and Zone binaries": ("build", "Build L1 and Zone"),
    "Build neobank fixture contracts": ("build", "Build Earn fixtures"),
    "Prepare or restore persistent Tempo L1 baseline": ("topology", "Restore realistic L1 state"),
    "Provision topology and run neobank workload": ("benchmark", "Run live customer journeys"),
    "Publish benchmark results": ("results", "Calculate latency and throughput"),
    "Upload specs, logs, and reports": ("results", "Package p99 and fee results"),
}


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def run_command(
    args: list[str], cwd: Path, timeout: float = 60.0, check: bool = True
) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        args,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    if check and completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or "command failed"
        raise RuntimeError(f"{args[0]} failed: {detail}")
    return completed


def read_json_command(args: list[str], cwd: Path, timeout: float = 60.0) -> Any:
    output = run_command(args, cwd=cwd, timeout=timeout).stdout
    try:
        return json.loads(output)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{args[0]} returned invalid JSON: {error}") from error


def safe_number(value: Any, fallback: float = 0.0) -> float:
    return float(value) if isinstance(value, (int, float)) else fallback


def fee_to_usd(value: Any) -> float:
    # Benchmark fees are paid in an 18-decimal USD-denominated fee token.
    return safe_number(value) / 1_000_000_000_000_000_000


def report_to_result(report: dict[str, Any]) -> dict[str, Any]:
    """Convert a txgen scenario report into the small UI result contract."""
    latency = report.get("client_observed_e2e_latency") or report.get(
        "total_scenario_latency", {}
    )
    completed = int(report.get("completed", 0))
    elapsed_ms = safe_number(report.get("elapsed_ms"))
    completed_rate = safe_number(report.get("completed_scenarios_per_second"))
    elapsed_seconds = (
        completed / completed_rate
        if completed > 0 and completed_rate > 0
        else elapsed_ms / 1_000
    )

    raw_steps = report.get("steps") or []
    raw_receipts = report.get("receipt_metrics") or []
    receipts_by_step = {
        metric.get("labels", {}).get("step"): metric
        for metric in raw_receipts
        if metric.get("labels", {}).get("step")
    }
    steps_by_name = {step.get("name"): step for step in raw_steps if step.get("name")}

    steps: list[dict[str, Any]] = []
    submit_count = 0
    for step in raw_steps:
        if step.get("kind") == "checkpoint":
            continue
        name = str(step.get("name", "unnamed-step"))
        command_latency = step.get("command_latency") or step.get("latency") or {}
        receipt = receipts_by_step.get(name, {})
        fee_paid = receipt.get("fee_paid") or {}
        gas_used = receipt.get("gas_used") or {}
        if step.get("kind") == "submit":
            submit_count += int(step.get("success", 0))
        steps.append(
            {
                "id": step.get("id") or name,
                "name": name,
                "label": FRIENDLY_STEP_NAMES.get(name, name.replace("_", " ").replace(".", " · ")),
                "chain": step.get("chain") or "local",
                "kind": step.get("kind") or "unknown",
                "success": int(step.get("success", 0)),
                "failed": int(step.get("failed", 0)),
                "meanMs": safe_number(command_latency.get("mean_ms")),
                "p50Ms": safe_number(command_latency.get("p50_ms")),
                "p95Ms": safe_number(command_latency.get("p95_ms")),
                "p99Ms": safe_number(command_latency.get("p99_ms")),
                "meanGas": safe_number(gas_used.get("mean")),
                "p99Gas": safe_number(gas_used.get("p99")),
                "meanCostUsd": fee_to_usd(fee_paid.get("mean")),
                "p99CostUsd": fee_to_usd(fee_paid.get("p99")),
            }
        )

    phases: list[dict[str, Any]] = []
    for phase in PHASES:
        phase_steps = [
            step
            for step in steps
            if any(step["name"].startswith(prefix) for prefix in phase["prefixes"])
        ]
        critical_mean_ms = sum(
            safe_number(
                (
                    steps_by_name.get(step_name, {}).get("command_latency")
                    or steps_by_name.get(step_name, {}).get("latency")
                    or {}
                ).get("mean_ms")
            )
            for step_name in phase["critical"]
        )
        terminal = next(
            (step for step in phase_steps if step["name"] == phase["terminal"]), None
        )
        phase_receipts = [
            metric
            for metric in raw_receipts
            if any(
                str(metric.get("labels", {}).get("step", "")).startswith(prefix)
                for prefix in phase["prefixes"]
            )
        ]
        total_phase_fees = sum(
            safe_number((metric.get("fee_paid") or {}).get("mean"))
            * int((metric.get("fee_paid") or {}).get("count", 0))
            for metric in phase_receipts
        )
        phases.append(
            {
                "id": phase["id"],
                "title": phase["title"],
                "eyebrow": phase["eyebrow"],
                "averageMs": critical_mean_ms,
                "terminalP99Ms": terminal["p99Ms"] if terminal else 0.0,
                "meanCostUsd": total_phase_fees / completed / 1e18 if completed else 0.0,
                "steps": phase_steps,
            }
        )

    total_fees = sum(
        safe_number((metric.get("fee_paid") or {}).get("mean"))
        * int((metric.get("fee_paid") or {}).get("count", 0))
        for metric in raw_receipts
    )
    total_gas = sum(
        safe_number((metric.get("gas_used") or {}).get("mean"))
        * int((metric.get("gas_used") or {}).get("count", 0))
        for metric in raw_receipts
    )
    receipt_count = sum(
        int((metric.get("gas_used") or {}).get("count", 0)) for metric in raw_receipts
    )

    return {
        "scenario": report.get("scenario", "neobank-private-zone-flow"),
        "reportVersion": int(report.get("version", 0)),
        "summary": {
            "started": int(report.get("started", 0)),
            "completed": completed,
            "failed": int(report.get("failed", 0)),
            "timedOut": int(report.get("timed_out", 0)),
            "elapsedSeconds": elapsed_seconds,
            "journeysPerSecond": completed_rate,
            "journeysPerMinute": completed_rate * 60,
            "submitTps": submit_count / elapsed_seconds if elapsed_seconds > 0 else 0.0,
            "submittedTransactions": submit_count,
            "receiptCount": receipt_count,
            "maximumInFlight": int(report.get("maximum_in_flight", 0)),
            "meanMs": safe_number(latency.get("mean_ms")),
            "p50Ms": safe_number(latency.get("p50_ms")),
            "p95Ms": safe_number(latency.get("p95_ms")),
            "p99Ms": safe_number(latency.get("p99_ms")),
            "meanJourneyCostUsd": total_fees / completed / 1e18 if completed else 0.0,
            "totalRunCostUsd": total_fees / 1e18,
            "meanJourneyGas": total_gas / completed if completed else 0.0,
            "totalGas": total_gas,
        },
        "phases": phases,
        "steps": steps,
        "configuration": report.get("configuration") or {},
    }


class BenchmarkController:
    def __init__(self, root: Path, state_dir: Path, branch: str, demo: bool = False):
        self.root = root
        self.state_dir = state_dir
        self.branch = branch
        self.demo = demo
        self.lock = threading.RLock()
        self.worker: threading.Thread | None = None
        self._server_info_cache: dict[str, Any] | None = None
        self._server_info_checked_at = 0.0
        self.state_file = state_dir / "state.json"
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self.state = self._load_state()
        self.state.setdefault("scenarioResults", {})
        self.state["server"] = self.server_info()
        if self.state.get("status") in {"queued", "running"}:
            self.state["status"] = "interrupted"
            self.state["error"] = (
                "The local UI stopped while the remote run was active. Open the linked "
                "Actions run or start a new benchmark."
            )
            self._persist()

    def _load_state(self) -> dict[str, Any]:
        if self.state_file.is_file():
            try:
                loaded = json.loads(self.state_file.read_text())
                if isinstance(loaded, dict) and loaded.get("version") == STATE_VERSION:
                    return loaded
            except (OSError, json.JSONDecodeError):
                pass
        return {
            "version": STATE_VERSION,
            "status": "idle",
            "stage": "ready",
            "message": "Ready for a live benchmark",
            "run": None,
            "result": None,
            "scenarioResults": {},
            "history": [],
            "error": None,
            "updatedAt": utc_now(),
        }

    def _persist(self) -> None:
        self.state["updatedAt"] = utc_now()
        temporary = self.state_file.with_suffix(".tmp")
        temporary.write_text(json.dumps(self.state, indent=2, sort_keys=True))
        temporary.replace(self.state_file)

    def _update(self, **values: Any) -> None:
        with self.lock:
            self.state.update(values)
            self._persist()

    def snapshot(self) -> dict[str, Any]:
        with self.lock:
            self.state["server"] = self.server_info()
            return copy.deepcopy(self.state)

    def server_info(self, force: bool = False) -> dict[str, Any]:
        if (
            not force
            and self._server_info_cache is not None
            and time.monotonic() - self._server_info_checked_at < 15
        ):
            return copy.deepcopy(self._server_info_cache)
        remote_branch = False
        authenticated = False
        if not self.demo:
            remote_branch = (
                run_command(
                    [
                        "git",
                        "show-ref",
                        "--verify",
                        "--quiet",
                        f"refs/remotes/origin/{self.branch}",
                    ],
                    self.root,
                    timeout=15,
                    check=False,
                ).returncode
                == 0
            )
            authenticated = (
                run_command(["gh", "auth", "status"], self.root, timeout=15, check=False).returncode
                == 0
            )
        info = {
            "repository": REPOSITORY,
            "branch": self.branch,
            "workflowUrl": WORKFLOW_URL,
            "remoteBranchAvailable": remote_branch or self.demo,
            "authenticated": authenticated or self.demo,
            "demoMode": self.demo,
            "scenarios": [
                {"id": scenario_id, **definition}
                for scenario_id, definition in SCENARIOS.items()
            ],
        }
        self._server_info_cache = info
        self._server_info_checked_at = time.monotonic()
        return copy.deepcopy(info)

    def start(self, raw_config: dict[str, Any]) -> dict[str, Any]:
        with self.lock:
            if self.worker and self.worker.is_alive():
                raise ValueError("A benchmark is already running")
            config = self._validate_config(raw_config)
            info = self.server_info(force=True)
            if not info["authenticated"]:
                raise ValueError("GitHub CLI is not authenticated; run `gh auth login` first")
            if not info["remoteBranchAvailable"]:
                raise ValueError(
                    f"Branch `{self.branch}` is not on origin yet; push it before "
                    "starting the remote benchmark"
                )
            self.state.update(
                {
                    "status": "queued",
                    "stage": "request",
                    "message": "Sending benchmark request",
                    "run": {
                        "id": None,
                        "url": None,
                        "branch": self.branch,
                        "config": config,
                        "scenario": config["scenario"],
                        "preset": config["preset"],
                        "startedAt": utc_now(),
                        "actionSteps": [],
                    },
                    "result": None,
                    "error": None,
                }
            )
            self._persist()
            target = self._run_demo if self.demo else self._dispatch_and_follow
            self.worker = threading.Thread(target=target, args=(config,), daemon=True)
            self.worker.start()
            return copy.deepcopy(self.state)

    def cancel(self) -> dict[str, Any]:
        with self.lock:
            run_id = (self.state.get("run") or {}).get("id")
            if not run_id:
                raise ValueError("There is no dispatched run to cancel")
        if not self.demo:
            run_command(
                ["gh", "run", "cancel", str(run_id), "--repo", REPOSITORY],
                self.root,
                timeout=30,
            )
        self._update(status="cancelling", message="Cancelling the benchmark")
        return self.snapshot()

    @staticmethod
    def _validate_config(raw: dict[str, Any]) -> dict[str, Any]:
        def integer(name: str, default: int, minimum: int, maximum: int) -> int:
            try:
                value = int(raw.get(name, default))
            except (TypeError, ValueError) as error:
                raise ValueError(f"{name} must be a whole number") from error
            if not minimum <= value <= maximum:
                raise ValueError(f"{name} must be between {minimum} and {maximum}")
            return value

        count = integer("count", 100, 1, 1_000)
        concurrency = integer("concurrency", 12, 1, 100)
        accounts = integer(
            "accounts", max(100, count, concurrency), max(count, concurrency), 10_000
        )
        scenario = str(raw.get("scenario", "deposit"))
        definition = SCENARIOS.get(scenario)
        if definition is None:
            raise ValueError(f"Unknown benchmark scenario: {scenario}")
        try:
            rate = float(raw.get("rate", 20))
        except (TypeError, ValueError) as error:
            raise ValueError("rate must be a number") from error
        if not 0.1 <= rate <= 1_000:
            raise ValueError("rate must be between 0.1 and 1000")
        state_bloat = str(raw.get("stateBloat", "1"))
        if state_bloat not in {"0", "1", "10", "100"}:
            raise ValueError("stateBloat must be 0, 1, 10, or 100")
        return {
            "scenario": scenario,
            "preset": definition["preset"],
            "count": count,
            "accounts": accounts,
            "rate": rate,
            "concurrency": concurrency,
            "stateBloat": state_bloat,
        }

    def _dispatch_and_follow(self, config: dict[str, Any]) -> None:
        try:
            previous = {
                int(run["databaseId"])
                for run in self._list_dispatch_runs()
                if run.get("databaseId")
            }
            dispatched_at = datetime.now(timezone.utc)
            command = [
                "gh",
                "workflow",
                "run",
                WORKFLOW,
                "--repo",
                REPOSITORY,
                "--ref",
                self.branch,
                "--field",
                f"preset={config['preset']}",
                "--field",
                f"accounts={config['accounts']}",
                "--field",
                f"count={config['count']}",
                "--field",
                f"tps={config['rate']}",
                "--field",
                f"max-concurrent={config['concurrency']}",
                "--field",
                f"state-bloat-gib={config['stateBloat']}",
            ]
            run_command(command, self.root, timeout=30)
            self._update(message="Waiting for a benchmark runner")
            run = self._find_new_run(previous, dispatched_at)
            run_id = int(run["databaseId"])
            with self.lock:
                self.state["run"].update(
                    {"id": run_id, "url": run.get("url"), "headSha": run.get("headSha")}
                )
                self._persist()
            self._follow_run(run_id)
        except Exception as error:  # surfaced verbatim to the local operator
            self._update(
                status="failed",
                stage="failed",
                message="Benchmark failed",
                error=str(error),
            )

    def _list_dispatch_runs(self) -> list[dict[str, Any]]:
        data = read_json_command(
            [
                "gh",
                "run",
                "list",
                "--repo",
                REPOSITORY,
                "--workflow",
                WORKFLOW,
                "--branch",
                self.branch,
                "--event",
                "workflow_dispatch",
                "--limit",
                "20",
                "--json",
                "databaseId,status,conclusion,createdAt,url,headBranch,headSha",
            ],
            self.root,
        )
        return data if isinstance(data, list) else []

    def _find_new_run(
        self, previous: set[int], dispatched_at: datetime
    ) -> dict[str, Any]:
        deadline = time.monotonic() + 60
        earliest = dispatched_at - timedelta(seconds=10)
        while time.monotonic() < deadline:
            for run in self._list_dispatch_runs():
                run_id = int(run.get("databaseId", 0))
                created = datetime.fromisoformat(str(run["createdAt"]).replace("Z", "+00:00"))
                if run_id not in previous and created >= earliest:
                    return run
            time.sleep(2)
        raise RuntimeError("GitHub accepted the dispatch, but the new Actions run did not appear")

    def _follow_run(self, run_id: int) -> None:
        while True:
            run = read_json_command(
                [
                    "gh",
                    "run",
                    "view",
                    str(run_id),
                    "--repo",
                    REPOSITORY,
                    "--json",
                    "databaseId,status,conclusion,url,createdAt,updatedAt,headSha,jobs",
                ],
                self.root,
            )
            action_steps, stage, message = self._action_progress(run)
            with self.lock:
                self.state["status"] = (
                    "running" if run.get("status") != "completed" else "processing"
                )
                self.state["stage"] = stage
                self.state["message"] = message
                self.state["run"].update(
                    {
                        "url": run.get("url"),
                        "headSha": run.get("headSha"),
                        "actionSteps": action_steps,
                        "githubStatus": run.get("status"),
                        "githubConclusion": run.get("conclusion"),
                    }
                )
                self._persist()
            if run.get("status") == "completed":
                if run.get("conclusion") != "success":
                    raise RuntimeError(
                        "GitHub Actions finished with "
                        f"{run.get('conclusion') or 'an unknown failure'}; open the run for logs"
                    )
                break
            time.sleep(3)

        self._update(stage="results", message="Downloading the txgen report")
        result = self._download_and_parse(run_id)
        with self.lock:
            run_record = copy.deepcopy(self.state["run"])
            run_record["finishedAt"] = utc_now()
            history_entry = {
                "id": run_record.get("id"),
                "url": run_record.get("url"),
                "headSha": run_record.get("headSha"),
                "startedAt": run_record.get("startedAt"),
                "finishedAt": run_record.get("finishedAt"),
                "config": run_record.get("config"),
                "scenario": run_record.get("scenario"),
                "summary": result.get("summary"),
            }
            history = [history_entry] + [
                item for item in self.state.get("history", []) if item.get("id") != run_id
            ]
            scenario_results = copy.deepcopy(self.state.get("scenarioResults", {}))
            scenario_results[run_record["scenario"]] = {
                "run": run_record,
                "result": result,
            }
            self.state.update(
                {
                    "status": "completed",
                    "stage": "complete",
                    "message": "Results ready",
                    "run": run_record,
                    "result": result,
                    "scenarioResults": scenario_results,
                    "history": history[:8],
                    "error": None,
                }
            )
            self._persist()

    @staticmethod
    def _action_progress(run: dict[str, Any]) -> tuple[list[dict[str, Any]], str, str]:
        visible: list[dict[str, Any]] = []
        for job in run.get("jobs") or []:
            for step in job.get("steps") or []:
                name = step.get("name")
                if name not in DISPLAY_JOB_STEPS:
                    continue
                stage, label = DISPLAY_JOB_STEPS[name]
                visible.append(
                    {
                        "stage": stage,
                        "name": label,
                        "status": step.get("status"),
                        "conclusion": step.get("conclusion"),
                    }
                )
        active = next((step for step in visible if step["status"] == "in_progress"), None)
        if active:
            return visible, active["stage"], active["name"]
        pending = next((step for step in visible if step["status"] == "queued"), None)
        if pending:
            return visible, pending["stage"], f"Up next: {pending['name']}"
        if run.get("status") == "queued":
            return visible, "request", "Waiting for a benchmark runner"
        return visible, "request", "Preparing benchmark"

    def _download_and_parse(self, run_id: int) -> dict[str, Any]:
        artifacts = read_json_command(
            ["gh", "api", f"repos/{REPOSITORY}/actions/runs/{run_id}/artifacts"],
            self.root,
        ).get("artifacts", [])
        preset = (self.state.get("run") or {}).get("preset")
        expected_prefix = f"zones-benchmark-neobank-{preset}-{run_id}-"
        artifact = next(
            (
                item
                for item in artifacts
                if str(item.get("name", "")).startswith(expected_prefix)
                and not item.get("expired", False)
            ),
            None,
        )
        if artifact is None:
            raise RuntimeError("The successful workflow did not upload its benchmark artifact")
        destination = self.state_dir / "runs" / str(run_id)
        if destination.exists():
            shutil.rmtree(destination)
        destination.mkdir(parents=True)
        run_command(
            [
                "gh",
                "run",
                "download",
                str(run_id),
                "--repo",
                REPOSITORY,
                "--name",
                str(artifact["name"]),
                "--dir",
                str(destination),
            ],
            self.root,
            timeout=180,
        )
        reports = list(destination.rglob("report-neobank-e2e.json"))
        if len(reports) != 1:
            raise RuntimeError(f"Expected one txgen report in the artifact, found {len(reports)}")
        report = json.loads(reports[0].read_text())
        result = report_to_result(report)
        result["artifactName"] = artifact["name"]
        return result

    def _run_demo(self, config: dict[str, Any]) -> None:
        try:
            fake_id = int(time.time())
            with self.lock:
                self.state["run"].update(
                    {
                        "id": fake_id,
                        "url": WORKFLOW_URL,
                        "headSha": "demo",
                    }
                )
                self._persist()
            labels = (
                ("build", "Build L1 and Zone"),
                ("topology", "Start the private Zone"),
                ("benchmark", "Run live customer journeys"),
                ("results", "Calculate p99 and fees"),
            )
            action_steps: list[dict[str, Any]] = []
            for stage, label in labels:
                action_steps.append(
                    {"stage": stage, "name": label, "status": "in_progress", "conclusion": None}
                )
                self._update(
                    status="running",
                    stage=stage,
                    message=label,
                    run={
                        **self.state["run"],
                        "actionSteps": copy.deepcopy(action_steps),
                    },
                )
                time.sleep(0.45)
                action_steps[-1].update(status="completed", conclusion="success")
            result = demo_result(config)
            with self.lock:
                run_record = copy.deepcopy(self.state["run"])
                run_record["actionSteps"] = action_steps
                run_record["finishedAt"] = utc_now()
                history_entry = {
                    "id": run_record.get("id"),
                    "url": run_record.get("url"),
                    "headSha": run_record.get("headSha"),
                    "startedAt": run_record.get("startedAt"),
                    "finishedAt": run_record.get("finishedAt"),
                    "config": run_record.get("config"),
                    "scenario": run_record.get("scenario"),
                    "summary": result.get("summary"),
                }
                scenario_results = copy.deepcopy(self.state.get("scenarioResults", {}))
                scenario_results[run_record["scenario"]] = {
                    "run": run_record,
                    "result": result,
                }
                self.state.update(
                    {
                        "status": "completed",
                        "stage": "complete",
                        "message": "Demo results ready",
                        "run": run_record,
                        "result": result,
                        "scenarioResults": scenario_results,
                        "history": [history_entry, *self.state.get("history", [])][:8],
                        "error": None,
                    }
                )
                self._persist()
        except Exception as error:
            self._update(status="failed", stage="failed", message="Demo failed", error=str(error))


def demo_result(config: dict[str, Any]) -> dict[str, Any]:
    step_values = (
        ("onramp.encryption", "l1", "invoke", 0.43, 0.60, 0.0, 0.0),
        ("onramp.submission", "l1", "submit", 0.27, 0.53, 95_051.0, 0.00005703),
        ("onramp.enqueued", "l1", "wait_log", 368.0, 564.0, 0.0, 0.0),
        ("onramp.zone_deposit.processed", "zone", "wait_log", 411.0, 517.0, 0.0, 0.0),
        ("earn_deposit.encryption", "l1", "invoke", 0.55, 0.94, 0.0, 0.0),
        ("earn_deposit.request", "zone", "submit", 0.50, 0.88, 1_229_232.0, 0.0),
        ("earn_deposit.request_result", "zone", "wait_log", 684.0, 1498.0, 0.0, 0.0),
        ("earn_deposit.l1_processed_locator", "l1", "wait_log", 1740.0, 2555.0, 0.0, 0.0),
        ("earn_deposit.l1_result", "l1", "wait_log", 2.02, 7.75, 0.0, 0.0),
        ("earn_deposit.zone_return.processed", "zone", "wait_log", 375.0, 512.0, 0.0, 0.0),
        ("earn_redeem.encryption", "l1", "invoke", 0.58, 1.26, 0.0, 0.0),
        ("earn_redeem.request", "zone", "submit", 0.52, 0.83, 692_645.0, 0.0),
        ("earn_redeem.request_result", "zone", "wait_log", 675.0, 1007.0, 0.0, 0.0),
        ("earn_redeem.l1_processed_locator", "l1", "wait_log", 1692.0, 2062.0, 0.0, 0.0),
        ("earn_redeem.l1_result", "l1", "wait_log", 1.84, 4.92, 0.0, 0.0),
        ("earn_redeem.zone_return.processed", "zone", "wait_log", 424.0, 513.0, 0.0, 0.0),
        ("offramp", "zone", "submit", 0.47, 0.70, 162_759.0, 0.0),
        ("offramp_result", "zone", "wait_log", 684.0, 1015.0, 0.0, 0.0),
        ("offramp_processed", "l1", "wait_log", 1709.0, 2067.0, 0.0, 0.0),
    )
    steps = [
        {
            "id": name,
            "name": name,
            "label": FRIENDLY_STEP_NAMES.get(name, name),
            "chain": chain,
            "kind": kind,
            "success": config["count"],
            "failed": 0,
            "meanMs": mean,
            "p50Ms": mean,
            "p95Ms": p99 * 0.9,
            "p99Ms": p99,
            "meanGas": gas,
            "p99Gas": gas * 1.05 if gas else 0.0,
            "meanCostUsd": cost,
            "p99CostUsd": cost,
        }
        for name, chain, kind, mean, p99, gas, cost in step_values
    ]
    phase_values = (
        ("deposit", "Deposit into the Zone", "L1 → private Zone", 780.0, 517.0, 0.00005703),
        ("earn_deposit", "Put funds into Earn", "Zone → L1 Earn → Zone", 2119.0, 512.0, 0.0),
        (
            "earn_redeem",
            "Redeem vault shares",
            "Zone → L1 redeem → Zone",
            2119.0,
            513.0,
            0.0,
        ),
        ("withdraw", "Withdraw back to L1", "Private Zone → L1 wallet", 1709.0, 2067.0, 0.0),
    )
    phases = []
    for phase_id, title, eyebrow, average, p99, cost in phase_values:
        phase_definition = next(phase for phase in PHASES if phase["id"] == phase_id)
        phases.append(
            {
                "id": phase_id,
                "title": title,
                "eyebrow": eyebrow,
                "averageMs": average,
                "terminalP99Ms": p99,
                "meanCostUsd": cost,
                "steps": [
                    step
                    for step in steps
                    if any(
                        step["name"].startswith(prefix)
                        for prefix in phase_definition["prefixes"]
                    )
                ],
            }
        )
    scenario_id = config["scenario"]
    definition = SCENARIOS[scenario_id]
    selected_phase = next(phase for phase in phases if phase["id"] == definition["phase"])
    selected_steps = selected_phase["steps"]
    demo_stats = {
        "deposit": {"rate": 10.8, "mean": 780.0, "p50": 782.0, "p95": 1031.0, "p99": 1084.0},
        "earn_deposit": {"rate": 4.1, "mean": 2119.0, "p50": 2128.0, "p95": 2824.0, "p99": 3068.0},
        "earn_redeem": {"rate": 4.0, "mean": 2119.0, "p50": 2132.0, "p95": 2861.0, "p99": 3072.0},
        "withdraw": {"rate": 5.7, "mean": 1709.0, "p50": 1556.0, "p95": 2066.0, "p99": 2067.0},
    }[scenario_id]
    mean_gas = sum(step["meanGas"] for step in selected_steps)
    mean_cost = selected_phase["meanCostUsd"]
    elapsed_seconds = config["count"] / demo_stats["rate"]
    return {
        "scenario": definition["preset"],
        "reportVersion": 2,
        "summary": {
            "started": config["count"],
            "completed": config["count"],
            "failed": 0,
            "timedOut": 0,
            "elapsedSeconds": elapsed_seconds,
            "journeysPerSecond": demo_stats["rate"],
            "journeysPerMinute": demo_stats["rate"] * 60,
            "submitTps": config["count"] / elapsed_seconds,
            "submittedTransactions": config["count"],
            "receiptCount": config["count"],
            "maximumInFlight": config["concurrency"],
            "meanMs": demo_stats["mean"],
            "p50Ms": demo_stats["p50"],
            "p95Ms": demo_stats["p95"],
            "p99Ms": demo_stats["p99"],
            "meanJourneyCostUsd": mean_cost,
            "totalRunCostUsd": mean_cost * config["count"],
            "meanJourneyGas": mean_gas,
            "totalGas": mean_gas * config["count"],
        },
        "phases": [selected_phase],
        "steps": selected_steps,
        "configuration": {
            "requested_instances": config["count"],
            "starts_per_second": config["rate"],
            "maximum_in_flight": config["concurrency"],
        },
        "demo": True,
    }


class IterativeBenchHandler(BaseHTTPRequestHandler):
    controller: BenchmarkController
    static_dir: Path

    def do_GET(self) -> None:  # noqa: N802
        path = urlparse(self.path).path
        if path == "/api/state":
            self._json(self.controller.snapshot())
            return
        if path == "/api/health":
            self._json({"ok": True, "time": utc_now()})
            return
        files = {
            "/": ("index.html", "text/html; charset=utf-8"),
            "/index.html": ("index.html", "text/html; charset=utf-8"),
            "/app.js": ("app.js", "text/javascript; charset=utf-8"),
            "/styles.css": ("styles.css", "text/css; charset=utf-8"),
        }
        selected = files.get(path)
        if selected is None:
            self.send_error(HTTPStatus.NOT_FOUND)
            return
        filename, content_type = selected
        try:
            body = (self.static_dir / filename).read_bytes()
        except OSError:
            self.send_error(HTTPStatus.NOT_FOUND)
            return
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", content_type)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:  # noqa: N802
        path = urlparse(self.path).path
        try:
            origin = self.headers.get("Origin")
            host = self.headers.get("Host")
            if origin and urlparse(origin).netloc != host:
                self._json({"error": "Cross-origin requests are not allowed"}, HTTPStatus.FORBIDDEN)
                return
            if path == "/api/runs":
                self._json(self.controller.start(self._request_json()), HTTPStatus.ACCEPTED)
                return
            if path == "/api/runs/cancel":
                self._json(self.controller.cancel(), HTTPStatus.ACCEPTED)
                return
            self.send_error(HTTPStatus.NOT_FOUND)
        except ValueError as error:
            self._json({"error": str(error)}, HTTPStatus.CONFLICT)
        except Exception as error:
            self._json({"error": str(error)}, HTTPStatus.INTERNAL_SERVER_ERROR)

    def _request_json(self) -> dict[str, Any]:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError as error:
            raise ValueError("Invalid Content-Length") from error
        if length > 65_536:
            raise ValueError("Request body is too large")
        if length == 0:
            return {}
        try:
            payload = json.loads(self.rfile.read(length))
        except json.JSONDecodeError as error:
            raise ValueError("Request body must be valid JSON") from error
        if not isinstance(payload, dict):
            raise ValueError("Request body must be a JSON object")
        return payload

    def _json(self, payload: Any, status: HTTPStatus = HTTPStatus.OK) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, message: str, *args: Any) -> None:
        if os.environ.get("ITERATIVE_BENCH_HTTP_LOG") == "1":
            super().log_message(message, *args)


def current_branch(root: Path) -> str:
    override = os.environ.get("ITERATIVE_BENCH_REF")
    branch = override or run_command(["git", "branch", "--show-current"], root).stdout.strip()
    if not branch or not BRANCH_PATTERN.fullmatch(branch) or ".." in branch:
        raise RuntimeError(f"Unsafe or unavailable Git branch name: {branch!r}")
    return branch


def main() -> None:
    parser = argparse.ArgumentParser(description="Launch the iterative neobank benchmark UI")
    parser.add_argument("--host", default=os.environ.get("ITERATIVE_BENCH_HOST", "127.0.0.1"))
    parser.add_argument(
        "--port", type=int, default=int(os.environ.get("ITERATIVE_BENCH_PORT", "4179"))
    )
    parser.add_argument("--no-open", action="store_true")
    arguments = parser.parse_args()

    root = Path(__file__).resolve().parents[3]
    static_dir = Path(__file__).resolve().parent / "static"
    state_dir = Path(
        os.environ.get("ITERATIVE_BENCH_STATE_DIR", root / "target" / "iterative-bench")
    )
    demo = os.environ.get("ITERATIVE_BENCH_DEMO") == "1"
    controller = BenchmarkController(root, state_dir, current_branch(root), demo=demo)

    handler = type(
        "ConfiguredIterativeBenchHandler",
        (IterativeBenchHandler,),
        {"controller": controller, "static_dir": static_dir},
    )
    server = ThreadingHTTPServer((arguments.host, arguments.port), handler)
    url = f"http://{arguments.host}:{arguments.port}"
    print(f"Iterative benchmark UI: {url}")
    print(f"Workflow ref: {controller.branch}")
    if demo:
        print("Demo mode enabled; no GitHub workflow will be dispatched.")
    elif not controller.server_info()["remoteBranchAvailable"]:
        print(f"Note: push `{controller.branch}` to origin before pressing Run live benchmark.")
    if not arguments.no_open and os.environ.get("ITERATIVE_BENCH_NO_OPEN") != "1":
        threading.Timer(0.4, lambda: webbrowser.open(url)).start()
    try:
        server.serve_forever(poll_interval=0.25)
    except KeyboardInterrupt:
        print("\nStopping iterative benchmark UI")
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
