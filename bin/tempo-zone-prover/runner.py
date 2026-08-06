#!/usr/bin/env python3

import json
import os
import re
import signal
import subprocess
import time
from typing import Any, Dict, Optional


EIF_PATH = os.environ.get(
    "PROVER_EIF_PATH", "/opt/tempo-zone-prover/tempo-zone-prover.eif"
)
ENCLAVE_NAME = os.environ.get("ENCLAVE_NAME", "tempo-zone-prover")
ENCLAVE_CPU_COUNT = os.environ.get("ENCLAVE_CPU_COUNT", "2")
ENCLAVE_MEMORY_MIB = os.environ.get("ENCLAVE_MEMORY_MIB", "512")
ENCLAVE_CID = os.environ.get("ENCLAVE_CID", "16")
MONITOR_INTERVAL_SECONDS = int(os.environ.get("MONITOR_INTERVAL_SECONDS", "30"))

enclave_id: Optional[str] = None


def format_nitro_cli_error(detail: str) -> str:
    error_logs = []
    for path in dict.fromkeys(
        re.findall(r'"(/var/log/nitro_enclaves/[^"\n]+\.log)"', detail)
    ):
        try:
            with open(path, encoding="utf-8", errors="replace") as error_log:
                error_logs.append(
                    f"\nNitro Enclaves error log {path}:\n{error_log.read().strip()}"
                )
        except OSError as error:
            error_logs.append(f"\nUnable to read Nitro Enclaves error log {path}: {error}")

    return detail + "".join(error_logs)


def run_nitro_cli(
    *arguments: str, check: bool = True, stream: bool = False
) -> subprocess.CompletedProcess[str]:
    command = ["nitro-cli", *arguments]

    if stream:
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        output_lines = []
        assert process.stdout is not None
        for line in process.stdout:
            print(line, end="", flush=True)
            output_lines.append(line)

        returncode = process.wait()
        output = "".join(output_lines)
        if check and returncode != 0:
            detail = output.strip() or "no diagnostic output"
            raise RuntimeError(
                f"nitro-cli {' '.join(arguments)} failed: "
                f"{format_nitro_cli_error(detail)}"
            )

        return subprocess.CompletedProcess(command, returncode, output, "")

    try:
        return subprocess.run(
            command,
            check=check,
            capture_output=True,
            text=True,
        )
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() or error.stdout.strip() or "no diagnostic output"
        raise RuntimeError(
            f"nitro-cli {' '.join(arguments)} failed: {format_nitro_cli_error(detail)}"
        ) from error


def terminate_enclave() -> None:
    global enclave_id
    if not enclave_id:
        return

    print(f"Terminating enclave {enclave_id}", flush=True)
    run_nitro_cli("terminate-enclave", "--enclave-id", enclave_id, check=False)
    enclave_id = None


def handle_signal(signum: int, _frame: Any) -> None:
    terminate_enclave()
    raise SystemExit(128 + signum)


def launch_enclave() -> Dict[str, Any]:
    global enclave_id

    result = run_nitro_cli(
        "run-enclave",
        "--enclave-name",
        ENCLAVE_NAME,
        "--cpu-count",
        ENCLAVE_CPU_COUNT,
        "--memory",
        ENCLAVE_MEMORY_MIB,
        "--eif-path",
        EIF_PATH,
        "--enclave-cid",
        ENCLAVE_CID,
        stream=True,
    )

    output = result.stdout.strip()
    json_start = output.find("{")
    if json_start < 0:
        raise RuntimeError(f"nitro-cli returned no enclave description: {output}")

    description, _ = json.JSONDecoder().raw_decode(output[json_start:])
    enclave_id = description["EnclaveID"]
    return description


def enclave_is_running() -> bool:
    assert enclave_id is not None
    result = run_nitro_cli("describe-enclaves")
    descriptions = json.loads(result.stdout)
    return any(item.get("EnclaveID") == enclave_id for item in descriptions)


def main() -> None:
    if not os.path.isfile(EIF_PATH):
        raise FileNotFoundError(f"prover EIF not found: {EIF_PATH}")
    if MONITOR_INTERVAL_SECONDS < 1:
        raise ValueError("MONITOR_INTERVAL_SECONDS must be positive")

    signal.signal(signal.SIGTERM, handle_signal)
    signal.signal(signal.SIGINT, handle_signal)

    print("Launching the Tempo Zone prover as a non-debug Nitro Enclave", flush=True)
    description = launch_enclave()
    print(
        f"Tempo Zone prover enclave {description['EnclaveID']} is running "
        f"at CID {description['EnclaveCID']} on vsock port 5000",
        flush=True,
    )

    try:
        while True:
            time.sleep(MONITOR_INTERVAL_SECONDS)
            if not enclave_is_running():
                raise RuntimeError(f"enclave {enclave_id} stopped unexpectedly")
    finally:
        terminate_enclave()


if __name__ == "__main__":
    main()
