#!/usr/bin/env python3
"""Local UI server for real, isolated Tempo Zone benchmark scenarios."""

from __future__ import annotations

import argparse
import copy
import json
import os
import re
import shutil
import signal
import subprocess
import threading
import time
import webbrowser
from collections import deque
from datetime import datetime, timezone
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

STATE_VERSION = 4
BRANCH_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/-]*$")
STAGE_PREFIX = "LIVE_BENCH_STAGE "
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
        "route": "Earn Vault ↔ Zone",
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


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def run_command(
    args: list[str], cwd: Path, timeout: float = 60.0
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
    if completed.returncode != 0:
        detail = (
            completed.stderr.strip() or completed.stdout.strip() or "command failed"
        )
        raise RuntimeError(f"{args[0]} failed: {detail}")
    return completed


def safe_number(value: Any, fallback: float = 0.0) -> float:
    return float(value) if isinstance(value, (int, float)) else fallback


def fee_to_usd(value: Any) -> float:
    return safe_number(value) / 1_000_000_000_000_000_000


def report_to_result(report: dict[str, Any]) -> dict[str, Any]:
    """Convert an actual txgen scenario report into the UI result contract."""
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
                "label": FRIENDLY_STEP_NAMES.get(
                    name, name.replace("_", " ").replace(".", " · ")
                ),
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
                    steps_by_name.get(name, {}).get("command_latency")
                    or steps_by_name.get(name, {}).get("latency")
                    or {}
                ).get("mean_ms")
            )
            for name in phase["critical"]
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
        phase_fees = sum(
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
                "meanCostUsd": phase_fees / completed / 1e18 if completed else 0.0,
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
    def __init__(self, root: Path, state_dir: Path, branch: str):
        self.root = root
        self.state_dir = state_dir
        self.branch = branch
        self.lock = threading.RLock()
        self.worker: threading.Thread | None = None
        self.process: subprocess.Popen[str] | None = None
        self.cancel_requested = False
        self.state_file = state_dir / "state.json"
        self.runner = Path(__file__).with_name("local_runner.py")
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self.state = self._load_state()
        self.state.setdefault("scenarioResults", {})
        self.state["server"] = self.server_info()
        if self.state.get("status") in {
            "queued",
            "running",
            "processing",
            "cancelling",
        }:
            self.state.update(
                status="interrupted",
                stage="interrupted",
                message="The local benchmark process stopped",
                error="The UI server exited before its local benchmark completed; run the scenario again.",
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
            "message": "Ready to run locally",
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

    def server_info(self) -> dict[str, Any]:
        required = ["cargo", "forge", "cast", "jq", "curl", "bc", "git"]
        if not (
            self.root / "target" / "live-bench" / "deps" / "earn" / ".git"
        ).is_dir():
            required.append("gh")
        missing = [name for name in required if shutil.which(name) is None]
        return {
            "branch": self.branch,
            "localReady": not missing,
            "missingTools": missing,
            "runnerLabel": "Local Tempo + private Zone",
            "scenarios": [
                {"id": scenario_id, **definition}
                for scenario_id, definition in SCENARIOS.items()
            ],
        }

    def start(self, raw_config: dict[str, Any]) -> dict[str, Any]:
        with self.lock:
            if self.worker and self.worker.is_alive():
                raise ValueError("A benchmark is already running")
            config = self._validate_config(raw_config)
            info = self.server_info()
            if not info["localReady"]:
                raise ValueError(
                    "Missing local tools: " + ", ".join(info["missingTools"])
                )
            run_id = f"{int(time.time() * 1000)}-{config['scenario']}"
            self.cancel_requested = False
            self.state.update(
                {
                    "status": "queued",
                    "stage": "build",
                    "message": "Preparing the real local benchmark",
                    "run": {
                        "id": run_id,
                        "url": None,
                        "branch": self.branch,
                        "config": config,
                        "scenario": config["scenario"],
                        "preset": config["preset"],
                        "startedAt": utc_now(),
                    },
                    "result": None,
                    "error": None,
                }
            )
            self._persist()
            self.worker = threading.Thread(
                target=self._run_local, args=(config, run_id), daemon=True
            )
            self.worker.start()
            return copy.deepcopy(self.state)

    def cancel(self) -> dict[str, Any]:
        with self.lock:
            if not self.worker or not self.worker.is_alive():
                raise ValueError("There is no local benchmark to cancel")
            self.cancel_requested = True
            process = self.process
            self.state.update(
                status="cancelling", message="Stopping the local benchmark"
            )
            self._persist()
        if process and process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
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
        return {
            "scenario": scenario,
            "preset": definition["preset"],
            "count": count,
            "accounts": accounts,
            "rate": rate,
            "concurrency": concurrency,
        }

    def _run_local(self, config: dict[str, Any], run_id: str) -> None:
        destination = self.state_dir / "runs" / run_id
        report = destination / "report-neobank-e2e.json"
        destination.mkdir(parents=True, exist_ok=True)
        command = [
            "python3",
            str(self.runner),
            "--preset",
            str(config["preset"]),
            "--count",
            str(config["count"]),
            "--accounts",
            str(config["accounts"]),
            "--rate",
            str(config["rate"]),
            "--concurrency",
            str(config["concurrency"]),
            "--run-dir",
            str(destination),
            "--report",
            str(report),
        ]
        recent: deque[str] = deque(maxlen=30)
        try:
            process = subprocess.Popen(
                command,
                cwd=self.root,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                bufsize=1,
                start_new_session=True,
            )
            with self.lock:
                self.process = process
                self.state.update(status="running")
                self._persist()
            assert process.stdout is not None
            for raw_line in process.stdout:
                line = raw_line.rstrip()
                if line:
                    print(line, flush=True)
                    recent.append(line)
                if line.startswith(STAGE_PREFIX):
                    try:
                        progress = json.loads(line[len(STAGE_PREFIX) :])
                    except json.JSONDecodeError:
                        continue
                    self._update(
                        status="processing"
                        if progress.get("stage") == "results"
                        else "running",
                        stage=str(progress.get("stage", "benchmark")),
                        message=str(progress.get("message", "Running locally")),
                    )
            return_code = process.wait()
            with self.lock:
                self.process = None
            if return_code != 0:
                if self.cancel_requested or return_code in (130, -signal.SIGINT):
                    self._update(
                        status="interrupted",
                        stage="interrupted",
                        message="Local benchmark stopped",
                        error=None,
                    )
                    return
                useful = [
                    line for line in recent if not line.startswith("LIVE_BENCH_STAGE")
                ]
                detail = (
                    "\n".join(useful[-12:])
                    or f"local runner exited with code {return_code}"
                )
                raise RuntimeError(detail)
            if not report.is_file():
                raise RuntimeError("The local txgen run did not produce a report")
            result = report_to_result(json.loads(report.read_text()))
            self._complete(result)
        except Exception as error:
            self._update(
                status="failed",
                stage="failed",
                message="Local benchmark failed",
                error=str(error),
            )
        finally:
            with self.lock:
                self.process = None

    def _complete(self, result: dict[str, Any]) -> None:
        with self.lock:
            run_record = copy.deepcopy(self.state["run"])
            run_record["finishedAt"] = utc_now()
            history_entry = {
                "id": run_record.get("id"),
                "url": None,
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
                    "message": "Real local results ready",
                    "run": run_record,
                    "result": result,
                    "scenarioResults": scenario_results,
                    "history": [history_entry, *self.state.get("history", [])][:8],
                    "error": None,
                }
            )
            self._persist()

    def shutdown(self) -> None:
        with self.lock:
            process = self.process
            self.cancel_requested = True
        if process and process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGINT)
            except ProcessLookupError:
                pass


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
                self._json(
                    {"error": "Cross-origin requests are not allowed"},
                    HTTPStatus.FORBIDDEN,
                )
                return
            if path == "/api/runs":
                self._json(
                    self.controller.start(self._request_json()), HTTPStatus.ACCEPTED
                )
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
    branch = run_command(["git", "branch", "--show-current"], root).stdout.strip()
    if not branch or not BRANCH_PATTERN.fullmatch(branch) or ".." in branch:
        return "local-worktree"
    return branch


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Launch the real local neobank benchmark UI"
    )
    parser.add_argument(
        "--host", default=os.environ.get("ITERATIVE_BENCH_HOST", "127.0.0.1")
    )
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
    controller = BenchmarkController(root, state_dir, current_branch(root))
    handler = type(
        "ConfiguredIterativeBenchHandler",
        (IterativeBenchHandler,),
        {"controller": controller, "static_dir": static_dir},
    )
    server = ThreadingHTTPServer((arguments.host, arguments.port), handler)
    url = f"http://{arguments.host}:{arguments.port}"
    print(f"Real local benchmark UI: {url}")
    print(
        "Each Go button starts an isolated Tempo L1 + private Zone and runs txgen locally."
    )
    if not arguments.no_open and os.environ.get("ITERATIVE_BENCH_NO_OPEN") != "1":
        threading.Timer(0.4, lambda: webbrowser.open(url)).start()
    try:
        server.serve_forever(poll_interval=0.25)
    except KeyboardInterrupt:
        print("\nStopping local benchmark UI")
    finally:
        controller.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
