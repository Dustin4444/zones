#!/usr/bin/env python3
"""Run one real neobank benchmark preset against an isolated local Tempo + Zone."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from typing import Any

TEMPO_REF = "a80c1438241e9edd67d8a96c6c07d3697fcf77d8"
TXGEN_REF = "072877b673f60b5f559f17da098296f1841b6732"
DLUSD = "0x20C0000000000000000000000000000000000001"
PATHUSD = "0x20C0000000000000000000000000000000000000"
TIP403_REGISTRY = "0x403c000000000000000000000000000000000000"
# TIP403Registry's token_transfer_policies mapping is the fifth storage field.
TOKEN_TRANSFER_POLICIES_SLOT = 4
DEV_FAUCET_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
ACCOUNT_START = 16


class LocalBenchmark:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.root = Path(__file__).resolve().parents[3]
        self.cache = self.root / "target" / "live-bench"
        self.run_dir = args.run_dir.resolve()
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self.logs = self.run_dir / "logs"
        self.logs.mkdir(exist_ok=True)
        self.processes: list[tuple[str, subprocess.Popen[bytes], Any]] = []
        self.stopping = False

    def stage(self, stage: str, message: str) -> None:
        print(
            "LIVE_BENCH_STAGE " + json.dumps({"stage": stage, "message": message}),
            flush=True,
        )

    def command(
        self,
        argv: list[str | Path],
        *,
        cwd: Path | None = None,
        env: dict[str, str] | None = None,
    ) -> None:
        printable = " ".join(str(item) for item in argv[:3])
        print(f"local-bench command={printable}", flush=True)
        completed = subprocess.run(
            [str(item) for item in argv],
            cwd=cwd or self.root,
            env=env,
            check=False,
        )
        if completed.returncode != 0:
            raise RuntimeError(
                f"{Path(str(argv[0])).name} exited with code {completed.returncode}"
            )

    def output(
        self,
        argv: list[str | Path],
        *,
        cwd: Path | None = None,
        env: dict[str, str] | None = None,
    ) -> str:
        completed = subprocess.run(
            [str(item) for item in argv],
            cwd=cwd or self.root,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if completed.returncode != 0:
            detail = completed.stderr.strip() or completed.stdout.strip()
            raise RuntimeError(f"{Path(str(argv[0])).name} failed: {detail}")
        return completed.stdout.strip()

    def ensure_sources(self) -> tuple[Path, Path]:
        deps = self.cache / "deps"
        deps.mkdir(parents=True, exist_ok=True)
        tempo = deps / "tempo"
        if not (tempo / ".git").is_dir():
            self.command(
                ["git", "clone", "https://github.com/tempoxyz/tempo", str(tempo)],
                cwd=self.root,
            )
        try:
            self.command(["git", "checkout", "--detach", TEMPO_REF], cwd=tempo)
        except RuntimeError:
            self.command(["git", "fetch", "origin", TEMPO_REF], cwd=tempo)
            self.command(["git", "checkout", "--detach", TEMPO_REF], cwd=tempo)

        earn = deps / "earn"
        if not (earn / ".git").is_dir():
            if shutil.which("gh") is None:
                raise RuntimeError(
                    "`gh` is required once to clone the private tempoxyz/earn fixtures"
                )
            self.command(
                ["gh", "repo", "clone", "tempoxyz/earn", str(earn), "--", "--depth=1"],
                cwd=self.root,
            )
        else:
            self.command(["git", "fetch", "--depth=1", "origin", "main"], cwd=earn)
            self.command(["git", "checkout", "--detach", "FETCH_HEAD"], cwd=earn)
        self.command(["git", "submodule", "update", "--init", "--recursive"], cwd=earn)
        return tempo, earn

    def prepare(self) -> tuple[Path, Path, Path, str]:
        self.stage("build", "Building the real local benchmark binaries")
        for dependency in ("cargo", "forge", "cast", "jq", "curl", "bc", "git"):
            if shutil.which(dependency) is None:
                raise RuntimeError(f"missing required local command: {dependency}")
        tempo, earn = self.ensure_sources()
        self.command(
            [
                "cargo",
                "build",
                "--locked",
                "--release",
                "--bin",
                "tempo-zone",
                "--bin",
                "tempo-xtask",
            ]
        )
        build_env = dict(os.environ)
        build_env["CARGO_TARGET_DIR"] = str(self.root / "target")
        self.command(
            ["cargo", "build", "--locked", "--release", "--bin", "tempo"],
            cwd=tempo,
            env=build_env,
        )

        tools = self.cache / "tools"
        txgen = tools / "bin" / "txgen-tempo"
        if not txgen.is_file():
            self.command(
                [
                    "cargo",
                    "install",
                    "--root",
                    str(tools),
                    "--git",
                    "https://github.com/tempoxyz/txgen",
                    "--rev",
                    TXGEN_REF,
                    "--locked",
                    "txgen-tempo",
                ]
            )

        specs_out = self.cache / "specs-out"
        specs_out.mkdir(parents=True, exist_ok=True)
        self.command(
            [
                "forge",
                "build",
                "--root",
                "specs/ref-impls",
                "--skip",
                "test",
                "--skip",
                "script",
                "--out",
                str(specs_out),
            ]
        )
        self.command(
            [
                "forge",
                "build",
                "--root",
                str(earn),
                "--skip",
                "test",
                "--skip",
                "script",
                "--out",
                str(specs_out),
            ]
        )
        required = (
            "SingleZoneEarnRouter.sol/SingleZoneEarnRouter.json",
            "EarnVault.sol/EarnVault.json",
            "EarnFees.sol/EarnFees.json",
            "EarnFactory.sol/EarnFactory.json",
            "EarnContributionController.sol/EarnContributionController.json",
            "ERC4626Engine.sol/ERC4626Engine.json",
            "Simple4626Vault.sol/Simple4626Vault.json",
            "DemoTokenAuthority.sol/DemoTokenAuthority.json",
            "BridgeWalletFixture.sol/BridgeWalletFixture.json",
        )
        missing = [name for name in required if not (specs_out / name).is_file()]
        if missing:
            raise RuntimeError("missing fixture artifact: " + ", ".join(missing))
        earn_revision = self.output(["git", "rev-parse", "HEAD"], cwd=earn)
        return tempo, txgen, specs_out, earn_revision

    @staticmethod
    def reserve_ports(count: int) -> list[int]:
        sockets: list[socket.socket] = []
        try:
            for _ in range(count):
                handle = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                handle.bind(("127.0.0.1", 0))
                sockets.append(handle)
            return [int(handle.getsockname()[1]) for handle in sockets]
        finally:
            for handle in sockets:
                handle.close()

    @staticmethod
    def reserve_zone_ports() -> tuple[int, int]:
        for _ in range(100):
            base = LocalBenchmark.reserve_ports(1)[0]
            if base > 65_532:
                continue
            handles: list[socket.socket] = []
            try:
                for port in (base, base + 1, base + 2):
                    handle = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                    handle.bind(("127.0.0.1", port))
                    handles.append(handle)
                redacted = LocalBenchmark.reserve_ports(1)[0]
                return base, redacted
            except OSError:
                continue
            finally:
                for handle in handles:
                    handle.close()
        raise RuntimeError("could not reserve local Zone ports")

    def start_process(
        self, name: str, argv: list[str | Path], *, env: dict[str, str] | None = None
    ) -> subprocess.Popen[bytes]:
        log = (self.logs / f"{name}.log").open("wb")
        process = subprocess.Popen(
            [str(item) for item in argv],
            cwd=self.root,
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self.processes.append((name, process, log))
        time.sleep(0.25)
        if process.poll() is not None:
            log.flush()
            tail = (self.logs / f"{name}.log").read_text(errors="replace")[-4000:]
            raise RuntimeError(f"{name} stopped during startup:\n{tail}")
        return process

    def rpc(self, url: str, method: str, params: list[Any] | None = None) -> Any:
        body = json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []}
        ).encode()
        request = urllib.request.Request(
            url, data=body, headers={"Content-Type": "application/json"}
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            payload = json.loads(response.read())
        if payload.get("error"):
            raise RuntimeError(f"{method}: {payload['error']}")
        return payload.get("result")

    def wait_rpc(
        self,
        url: str,
        label: str,
        timeout: float = 90,
        process: subprocess.Popen[bytes] | None = None,
    ) -> None:
        deadline = time.monotonic() + timeout
        last_error: Exception | None = None
        while time.monotonic() < deadline:
            if process is not None and process.poll() is not None:
                log_path = self.logs / f"{label.lower().replace(' ', '-')}.log"
                if label == "Tempo L1":
                    log_path = self.logs / "tempo.log"
                elif label == "private Zone":
                    log_path = self.logs / "zone.log"
                tail = (
                    log_path.read_text(errors="replace")[-4000:]
                    if log_path.is_file()
                    else ""
                )
                raise RuntimeError(
                    f"{label} stopped during startup with code {process.returncode}:\n{tail}"
                )
            try:
                if self.rpc(url, "eth_chainId"):
                    return
            except (
                Exception
            ) as error:  # service is expected to be unavailable during startup
                last_error = error
            time.sleep(0.25)
        raise RuntimeError(f"timed out waiting for {label}: {last_error}")

    def derive(self, mnemonic_file: Path, index: int, private: bool = False) -> str:
        command = "private-key" if private else "address"
        return self.output(
            [
                "cast",
                "wallet",
                command,
                "--mnemonic",
                str(mnemonic_file),
                "--mnemonic-index",
                str(index),
            ]
        ).splitlines()[-1]

    def wait_token_balance(
        self, rpc_url: str, token: str, account: str, minimum: int
    ) -> None:
        deadline = time.monotonic() + 90
        last = "0"
        while time.monotonic() < deadline:
            try:
                last = self.output(
                    [
                        "cast",
                        "call",
                        token,
                        "balanceOf(address)(uint256)",
                        account,
                        "--rpc-url",
                        rpc_url,
                    ]
                ).split()[0]
                if int(last) >= minimum:
                    return
            except (RuntimeError, ValueError):
                pass
            time.sleep(0.25)
        raise RuntimeError(f"funding did not settle for {account}; balance={last}")

    def wait_zone_caught_up(
        self, zone_rpc: str, l1_rpc: str, timeout: float = 180
    ) -> None:
        """Drain setup blocks before txgen starts its measured scenario."""
        target = int(str(self.rpc(l1_rpc, "eth_blockNumber")), 16)
        deadline = time.monotonic() + timeout
        observed = 0
        while time.monotonic() < deadline:
            info = self.rpc(zone_rpc, "zone_getZoneInfo")
            raw = (
                info.get("tempoBlockNumber", "0x0") if isinstance(info, dict) else "0x0"
            )
            observed = int(str(raw), 16)
            if observed >= target:
                return
            time.sleep(0.25)
        raise RuntimeError(
            f"the Zone did not drain its local Tempo setup backlog; "
            f"processed={observed}, target={target}"
        )

    def topology(
        self, tempo: Path, specs_out: Path, earn_revision: str
    ) -> tuple[dict[str, str], Path]:
        self.stage("topology", "Starting a fresh local Tempo L1")
        mnemonic_file = self.run_dir / "mnemonic"
        mnemonic = json.loads(
            self.output(
                [
                    "cast",
                    "wallet",
                    "new-mnemonic",
                    "--words",
                    "12",
                    "--accounts",
                    "0",
                    "--json",
                ]
            )
        )["mnemonic"]
        mnemonic_file.write_text(mnemonic + "\n")
        mnemonic_file.chmod(0o600)
        control_address = self.derive(mnemonic_file, 0)
        sequencer_address = self.derive(mnemonic_file, 4)
        sequencer_key = self.derive(mnemonic_file, 4, private=True)
        users = [
            self.derive(mnemonic_file, index)
            for index in range(ACCOUNT_START, ACCOUNT_START + self.args.accounts)
        ]

        raw_genesis = (
            tempo / "crates" / "node" / "tests" / "assets" / "test-genesis.json"
        )
        if not raw_genesis.is_file():
            raise RuntimeError(f"pinned Tempo test genesis is missing: {raw_genesis}")
        copied_genesis = self.run_dir / "tempo-genesis-raw.json"
        patched_genesis = self.run_dir / "tempo-genesis.json"
        genesis = json.loads(raw_genesis.read_text())
        # Expiring Tempo transactions use wall-clock validity. Keep the dev chain's
        # genesis close to wall time so the faucet and txgen validity windows agree.
        genesis["timestamp"] = hex(int(time.time()))
        # dLUSD is a legacy token in Tempo's test genesis. Zone provisioning checks
        # the TIP-1092 registry binding, so explicitly bind it to allow-all just as
        # newly created TIP-20 tokens are bound by their factory transaction.
        policy_slot = self.output(
            ["cast", "index", "address", DLUSD, str(TOKEN_TRANSFER_POLICIES_SLOT)]
        )
        registry = genesis["alloc"][TIP403_REGISTRY]
        registry.setdefault("storage", {})[policy_slot] = f"0x{((1 << 64) | 1):064x}"
        copied_genesis.write_text(json.dumps(genesis))
        self.command(
            [
                self.root / "target" / "release" / "tempo-xtask",
                "install-reference-zone-factory",
                "--genesis",
                copied_genesis,
                "--output",
                patched_genesis,
                "--owner",
                sequencer_address,
                "--specs-out",
                specs_out,
            ]
        )

        l1_http_port, l1_ws_port, l1_p2p_port, l1_auth_port = self.reserve_ports(4)
        l1_http = f"http://127.0.0.1:{l1_http_port}"
        l1_ws = f"ws://127.0.0.1:{l1_ws_port}"
        tempo_process = self.start_process(
            "tempo",
            [
                self.root / "target" / "release" / "tempo",
                "node",
                "--chain",
                patched_genesis,
                "--dev",
                "--dev.block-time",
                "500ms",
                "--dev.finality-depth",
                "1",
                "--http",
                "--http.addr",
                "127.0.0.1",
                "--http.port",
                str(l1_http_port),
                "--http.api",
                "all",
                "--ws",
                "--ws.addr",
                "127.0.0.1",
                "--ws.port",
                str(l1_ws_port),
                "--ws.api",
                "all",
                "--port",
                str(l1_p2p_port),
                "--discovery.port",
                str(l1_p2p_port),
                "--authrpc.port",
                str(l1_auth_port),
                "--ipcdisable",
                "--disable-discovery",
                "--datadir",
                self.run_dir / "l1",
                "--log.file.directory",
                self.logs / "tempo-files",
                "--engine.disable-precompile-cache",
                "--builder.gaslimit",
                "500000000",
                "--faucet.enabled",
                "--faucet.private-key",
                DEV_FAUCET_KEY,
                "--faucet.amount",
                "1000000000000",
                "--faucet.address",
                PATHUSD,
                DLUSD,
                "--faucet.node-address",
                l1_http,
            ],
        )
        self.wait_rpc(l1_http, "Tempo L1", process=tempo_process)

        self.stage("topology", f"Funding {self.args.accounts} real benchmark accounts")
        for account in [control_address, sequencer_address, *users]:
            self.rpc(l1_http, "tempo_fundAddress", [account])
        self.wait_token_balance(l1_http, DLUSD, users[-1], 1_000_000_000_000)
        self.wait_token_balance(l1_http, PATHUSD, users[-1], 1_000_000_000_000)
        self.wait_token_balance(l1_http, DLUSD, control_address, 1_000_000_000_000)
        self.wait_token_balance(l1_http, PATHUSD, sequencer_address, 1_000_000_000_000)

        self.stage("topology", "Provisioning and starting the private Zone")
        zone_http_port, redacted_port = self.reserve_zone_ports()
        zone_http = f"http://127.0.0.1:{zone_http_port}"
        zone_ws = f"ws://127.0.0.1:{zone_http_port + 1}"
        allowed = [control_address, *users]
        zone_command: list[str | Path] = [
            self.root / "target" / "release" / "tempo-zone",
            "dev",
            "--l1.rpc-url",
            l1_ws,
            "--dev.key",
            sequencer_key,
            "--dev.token",
            DLUSD,
            "--dev.access-mode",
        ]
        zone_command.extend(
            (
                "--datadir",
                self.run_dir / "zone",
                "--http.port",
                str(zone_http_port),
                "--redacted-rpc.port",
                str(redacted_port),
                "--",
                "--zone.batch-interval-blocks",
                "5",
                "--withdrawal-poll-interval-secs",
                "1",
                "--engine.disable-sparse-trie-cache-pruning",
            )
        )
        zone_env = dict(os.environ)
        zone_env["DEV_KEY"] = sequencer_key
        zone_process = self.start_process("zone", zone_command, env=zone_env)
        self.wait_rpc(zone_http, "private Zone", timeout=60, process=zone_process)

        zone_json = self.run_dir / "zone" / "zone.json"
        deadline = time.monotonic() + 30
        while not zone_json.is_file() and time.monotonic() < deadline:
            time.sleep(0.2)
        if not zone_json.is_file():
            raise RuntimeError("tempo-zone dev did not write zone.json")
        zone = json.loads(zone_json.read_text())
        portal = str(zone["portal"])

        self.stage(
            "topology",
            f"Deploying Earn fixtures and authorizing {len(allowed)} benchmark accounts",
        )
        allowed_file = self.run_dir / "allowed-accounts"
        allowed_file.write_text("\n".join(allowed) + "\n")
        allowed_file.chmod(0o600)
        fixtures_file = self.run_dir / "neobank-fixtures.json"
        fixture_env = dict(os.environ)
        fixture_env.update(
            # The pinned test genesis assigns the legacy dLUSD/pathUSD admin
            # roles to the standard dev account. The Zone itself remains owned
            # by the independently generated sequencer account.
            FIXTURE_DEPLOYER_KEY=DEV_FAUCET_KEY,
            PORTAL_ADMIN_KEY=sequencer_key,
        )
        self.command(
            [
                self.root / "target" / "release" / "tempo-xtask",
                "deploy-neobank-fixtures",
                "--l1-rpc-url",
                l1_http,
                "--portal",
                portal,
                "--dlusd",
                DLUSD,
                "--pathusd",
                PATHUSD,
                "--private-asset",
                DLUSD,
                "--earn-revision",
                earn_revision,
                "--swap-mechanism",
                "direct-swap",
                "--liquidity",
                "10000000000",
                "--allowed-accounts-file",
                allowed_file,
                "--specs-out",
                specs_out,
                "--output",
                fixtures_file,
            ],
            env=fixture_env,
        )
        self.command(
            [
                self.root / "target" / "release" / "tempo-xtask",
                "configure-benchmark-fees",
                "--l1-rpc-url",
                l1_http,
                "--portal",
                portal,
                "--token",
                DLUSD,
                "--zone-gas-rate",
                "0",
                "--bounceback-gas",
                "0",
            ],
            env={**os.environ, "SEQUENCER_KEY": sequencer_key},
        )
        fixtures = json.loads(fixtures_file.read_text())
        earn_token = str(fixtures["earnToken"])
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            try:
                code = self.output(["cast", "code", earn_token, "--rpc-url", zone_http])
                if code != "0x":
                    break
            except RuntimeError:
                pass
            time.sleep(0.5)
        else:
            raise RuntimeError("the Zone did not ingest the Earn vault-share token")

        self.stage("topology", "Draining setup blocks before the measured run")
        self.wait_zone_caught_up(zone_http, l1_http)

        chain_id = int(str(self.rpc(l1_http, "eth_chainId")), 16)
        output_dir = self.run_dir / "output"
        output_dir.mkdir(exist_ok=True)
        report = self.args.report.resolve()
        report.parent.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ)
        env.update(
            {
                "L1_RPC_URL": l1_http,
                "L1_WS_RPC_URL": l1_ws,
                "ZONES_BENCH_L1_QUERY_RPC_URL": l1_http,
                "ZONES_BENCH_L1_SUBMIT_RPC_URLS": l1_http,
                "ZONE_RPC_URL": zone_http,
                "ZONE_WS_RPC_URL": zone_ws,
                "ZONE_REDACTED_RPC_URL": f"http://127.0.0.1:{redacted_port}",
                "L1_PORTAL_ADDRESS": portal,
                "ZONES_BENCH_TOKEN": DLUSD,
                "ZONES_BENCH_DLUSD": DLUSD,
                "ZONES_BENCH_PATHUSD": PATHUSD,
                "ZONES_BENCH_EXPECTED_L1_CHAIN_ID": str(chain_id),
                "ZONES_BENCH_EXPECTED_ZONE_CHAIN_ID": str(zone["chainId"]),
                "ZONES_BENCH_EXPECTED_ZONE_ID": str(zone["zoneId"]),
                "ZONES_BENCH_CONTROL_ACCOUNT_INDEX": "0",
                "ZONES_BENCH_CONTROL_ACCOUNT_END": "1",
                "ZONES_BENCH_SEQUENCER_ACCOUNT_INDEX": "4",
                "ZONES_BENCH_SEQUENCER_ACCOUNT_END": "5",
                "ZONES_BENCH_SEQUENCER_ADDRESS": sequencer_address,
                "ZONES_BENCH_ACCOUNT_START": str(ACCOUNT_START),
                "ZONES_BENCH_ACCOUNT_END": str(ACCOUNT_START + self.args.accounts),
                "ZONES_BENCH_ACCOUNTS": str(self.args.accounts),
                "ZONES_BENCH_COUNT": str(self.args.count),
                "ZONES_BENCH_TPS": str(self.args.rate),
                "ZONES_BENCH_MAX_CONCURRENT": str(self.args.concurrency),
                "ZONES_BENCH_NEOBANK_PRESET": self.args.preset,
                "ZONES_BENCH_MNEMONIC_FILE": str(mnemonic_file),
                "ZONES_BENCH_OUTPUT": str(output_dir),
                "ZONES_BENCH_REPORT": str(report),
                "ZONES_BENCH_RENDERED_SCENARIO": str(
                    output_dir / "scenario.rendered.yml"
                ),
                "ZONES_BENCH_RUN_ID": self.run_dir.name,
                "ZONES_BENCH_SEED": "42",
                "ZONES_BENCH_SAMPLE_INSTANCES": str(min(10, self.args.count)),
                "ZONES_BENCH_STEP_TIMEOUT": "10m",
                "ZONES_BENCH_SWAP_MECHANISM": "direct-swap",
                "ZONES_BENCH_SWAP_LIQUIDITY": "10000000000",
                "ZONES_BENCH_EARN_REVISION": earn_revision,
                "ZONES_BENCH_TEMPO_REF": TEMPO_REF,
                "ZONES_BENCH_TXGEN_REF": TXGEN_REF,
                "ZONES_BENCH_TARGET_ID": f"local-zone-{zone['zoneId']}",
                "ZONES_BENCH_INBOX": "0x1c00000000000000000000000000000000000001",
                "ZONES_BENCH_OUTBOX": "0x1c00000000000000000000000000000000000002",
                "ZONES_BENCH_L1_GAS_LIMIT": "500000000",
                "ZONES_BENCH_L1_GENERAL_GAS_LIMIT": "500000000",
                "ZONES_BENCH_WITHDRAWAL_MAX_BATCH_GAS": "100000000",
                "ZONES_BENCH_WITHDRAWAL_MAX_IN_FLIGHT_BATCHES": "4",
                "ZONES_BENCH_ZONE_BATCH_INTERVAL_BLOCKS": "5",
                "ZONES_BENCH_WITHDRAWAL_POLL_INTERVAL_SECS": "1",
                "ZONES_BENCH_SETUP_SETTLEMENT_TIMEOUT_SECS": "120",
                "ZONES_BENCH_DRAIN_TIMEOUT": "120",
                "ZONES_BENCH_FORCE_BLOAT": "0",
                "ZONES_BENCH_EARN_TOKEN": earn_token,
                "ZONES_BENCH_EARN_ROUTER": str(fixtures["earnRouter"]),
                "ZONES_BENCH_EARN_VAULT": str(fixtures["earnVault"]),
                "ZONES_BENCH_EARN_CONTRIBUTION_CONTROLLER": str(
                    fixtures["contributionController"]
                ),
                "ZONES_BENCH_BRIDGE_WALLET": str(fixtures["bridgeWallet"]),
                "ZONES_BENCH_GATEWAY": str(fixtures["gateway"]),
                "ZONES_BENCH_VAULT": str(fixtures["vault"]),
                "ZONES_BENCH_ENGINE": str(fixtures["engine"]),
                "ZONES_BENCH_VAULT_ADAPTER": str(fixtures["vaultAdapter"]),
                "ZONES_BENCH_REWARDS": str(fixtures["rewards"]),
                "TXGEN_TEMPO_BIN": str(self.cache / "tools" / "bin" / "txgen-tempo"),
                "RUNNER_TEMP": str(self.run_dir / "tmp"),
            }
        )
        Path(env["RUNNER_TEMP"]).mkdir(exist_ok=True)
        return env, report

    def run(self) -> None:
        tempo, txgen, specs_out, earn_revision = self.prepare()
        if not txgen.is_file():
            raise RuntimeError("txgen-tempo was not installed")
        env, report = self.topology(tempo, specs_out, earn_revision)
        self.stage(
            "benchmark", f"Running {self.args.count} real {self.args.preset} journeys"
        )
        self.command(["bash", "contrib/bench/run-neobank-private-flow.sh"], env=env)
        if not report.is_file():
            raise RuntimeError(f"txgen completed without writing {report}")
        parsed = json.loads(report.read_text())
        if (
            int(parsed.get("completed", 0)) != self.args.count
            or int(parsed.get("failed", 0)) != 0
        ):
            raise RuntimeError(
                "txgen report does not contain the requested successful journeys"
            )
        self.stage("results", "Reading the real txgen latency, gas, and fee report")

    def stop(self) -> None:
        if self.stopping:
            return
        self.stopping = True
        for _, process, _ in reversed(self.processes):
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGINT)
                except ProcessLookupError:
                    pass
        deadline = time.monotonic() + 8
        for _, process, log in reversed(self.processes):
            remaining = max(0.0, deadline - time.monotonic())
            try:
                process.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            log.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--preset", required=True)
    parser.add_argument("--count", type=int, required=True)
    parser.add_argument("--accounts", type=int, required=True)
    parser.add_argument("--rate", type=float, required=True)
    parser.add_argument("--concurrency", type=int, required=True)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    benchmark = LocalBenchmark(parse_args())

    def interrupt(_signum: int, _frame: Any) -> None:
        raise KeyboardInterrupt

    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)
    try:
        benchmark.run()
        return 0
    except KeyboardInterrupt:
        print("local-bench cancelled", file=sys.stderr, flush=True)
        return 130
    except Exception as error:
        print(f"local-bench error: {error}", file=sys.stderr, flush=True)
        return 1
    finally:
        benchmark.stop()


if __name__ == "__main__":
    raise SystemExit(main())
