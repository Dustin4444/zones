import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("server.py")
SPEC = importlib.util.spec_from_file_location("iterative_bench_server", MODULE_PATH)
assert SPEC and SPEC.loader
SERVER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SERVER)


def latency(mean: float, p99: float) -> dict:
    return {
        "samples": 2,
        "min_ms": mean / 2,
        "max_ms": p99,
        "mean_ms": mean,
        "p50_ms": mean,
        "p95_ms": p99,
        "p99_ms": p99,
    }


def step(name: str, chain: str, kind: str, mean: float, p99: float) -> dict:
    return {
        "id": name,
        "name": name,
        "chain": chain,
        "kind": kind,
        "success": 2,
        "failed": 0,
        "command_latency": latency(mean, p99),
    }


def receipt(name: str, gas: float, fee: float) -> dict:
    distribution = lambda value: {
        "count": 2,
        "min": value,
        "mean": value,
        "p50": value,
        "p95": value,
        "p99": value,
    }
    return {
        "labels": {"step": name},
        "gas_used": distribution(gas),
        "effective_gas_price": distribution(0),
        "fee_paid": distribution(fee),
    }


class ReportConversionTests(unittest.TestCase):
    def test_converts_four_action_journey(self) -> None:
        report = {
            "version": 2,
            "scenario": "neobank-private-zone-flow",
            "started": 2,
            "completed": 2,
            "failed": 0,
            "timed_out": 0,
            "elapsed_ms": 4_000,
            "completed_scenarios_per_second": 0.5,
            "maximum_in_flight": 2,
            "client_observed_e2e_latency": latency(3_000, 3_900),
            "steps": [
                step("onramp.submission", "l1", "submit", 100, 150),
                step("onramp.zone_deposit.processed", "zone", "wait_log", 500, 700),
                step("earn_deposit.request", "zone", "submit", 100, 150),
                step("earn_deposit.zone_return.processed", "zone", "wait_log", 700, 900),
                step("earn_redeem.request", "zone", "submit", 100, 150),
                step("earn_redeem.zone_return.processed", "zone", "wait_log", 700, 900),
                step("l1_before_offramp", "l1", "checkpoint", 10, 20),
                step("offramp", "zone", "submit", 100, 150),
                step("offramp_processed", "l1", "wait_log", 700, 900),
            ],
            "receipt_metrics": [
                receipt("onramp.submission", 100_000, 20_000_000_000_000),
                receipt("earn_deposit.request", 200_000, 0),
                receipt("earn_redeem.request", 300_000, 0),
                receipt("offramp", 400_000, 0),
            ],
            "configuration": {"requested_instances": 2},
        }

        result = SERVER.report_to_result(report)

        self.assertEqual(result["summary"]["submittedTransactions"], 8)
        self.assertEqual(result["summary"]["receiptCount"], 8)
        self.assertEqual(result["summary"]["p99Ms"], 3_900)
        self.assertEqual(result["summary"]["journeysPerMinute"], 30)
        self.assertAlmostEqual(result["summary"]["meanJourneyCostUsd"], 0.00002)
        self.assertEqual([phase["id"] for phase in result["phases"]], [
            "deposit", "earn_deposit", "earn_redeem", "withdraw"
        ])
        self.assertNotIn("private_transfer", {item["name"] for item in result["steps"]})


class ConfigurationTests(unittest.TestCase):
    def test_defaults_are_presentation_defaults(self) -> None:
        config = SERVER.BenchmarkController._validate_config({})
        self.assertEqual(config["scenario"], "deposit")
        self.assertEqual(config["preset"], "encrypted-deposit")
        self.assertEqual(config["count"], 100)
        self.assertEqual(config["rate"], 20)
        self.assertEqual(config["concurrency"], 12)
        self.assertEqual(config["accounts"], 100)

    def test_accounts_must_cover_concurrency(self) -> None:
        with self.assertRaisesRegex(ValueError, "accounts must be between 20 and 10000"):
            SERVER.BenchmarkController._validate_config(
                {"accounts": 10, "concurrency": 20, "count": 10}
            )

    def test_each_card_maps_to_a_standalone_preset(self) -> None:
        expected = {
            "deposit": "encrypted-deposit",
            "earn_deposit": "earn-deposit",
            "earn_redeem": "private-withdrawal",
            "withdraw": "zone-withdrawal",
        }
        for scenario, preset in expected.items():
            with self.subTest(scenario=scenario):
                config = SERVER.BenchmarkController._validate_config({"scenario": scenario})
                self.assertEqual(config["preset"], preset)

    def test_unknown_scenario_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "Unknown benchmark scenario"):
            SERVER.BenchmarkController._validate_config({"scenario": "not-real"})


if __name__ == "__main__":
    unittest.main()
