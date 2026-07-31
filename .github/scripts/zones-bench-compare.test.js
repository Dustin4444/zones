'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const test = require('node:test');

const {
  compareReports,
  extractMetrics,
  formatDelta,
  percentDelta,
} = require('./zones-bench-compare.js');

function report(overrides = {}) {
  return {
    version: 2,
    scenario: 'private-flow',
    completed: 100,
    failed: 2,
    timed_out: 1,
    completed_scenarios_per_second: 5,
    elapsed_ms: 30_000,
    maximum_in_flight: 12,
    steps: [
      { kind: 'submit', success: 80 },
      { kind: 'checkpoint', success: 100 },
      { kind: 'submit', success: 120 },
    ],
    client_observed_e2e_latency: {
      p50_ms: 100,
      p95_ms: 200,
      p99_ms: 300,
    },
    total_scenario_latency: {
      p50_ms: 110,
      p95_ms: 220,
      p99_ms: 330,
    },
    ...overrides,
  };
}

test('extracts comparison metrics using the measured journey window', () => {
  const metrics = extractMetrics(report(), 'baseline');

  assert.deepEqual(metrics, {
    scenario: 'private-flow',
    completed: 100,
    failed: 2,
    timedOut: 1,
    journeysPerSecond: 5,
    aggregateSubmitTps: 10,
    maximumInFlight: 12,
    latencyP50Ms: 100,
    latencyP95Ms: 200,
    latencyP99Ms: 300,
    latencySource: 'client_observed_e2e_latency',
  });
});

test('falls back to elapsed time and total scenario latency', () => {
  const metrics = extractMetrics(report({
    completed: 0,
    completed_scenarios_per_second: 0,
    elapsed_ms: 20_000,
    client_observed_e2e_latency: undefined,
  }));

  assert.equal(metrics.aggregateSubmitTps, 10);
  assert.equal(metrics.latencyP50Ms, 110);
  assert.equal(metrics.latencyP95Ms, 220);
  assert.equal(metrics.latencyP99Ms, 330);
  assert.equal(metrics.latencySource, 'total_scenario_latency');
});

test('formats signed deltas and reports a zero baseline as n/a', () => {
  assert.equal(percentDelta(10, 12), 20);
  assert.equal(formatDelta(10, 12), '+20.00%');
  assert.equal(formatDelta(10, 8), '-20.00%');
  assert.equal(formatDelta(10, 10), '+0.00%');
  assert.equal(formatDelta(0, 5), 'n/a');
});

test('renders all requested comparison rows', () => {
  const markdown = compareReports(
    report(),
    report({
      completed: 110,
      failed: 1,
      timed_out: 0,
      completed_scenarios_per_second: 5.5,
      maximum_in_flight: 14,
      steps: [{ kind: 'submit', success: 242 }],
      client_observed_e2e_latency: { p50_ms: 90, p95_ms: 180, p99_ms: 270 },
    }),
    { baselineLabel: 'main', featureLabel: 'topic|branch' },
  );

  assert.match(markdown, /^# Zones benchmark comparison/m);
  assert.match(markdown, /\| Metric \| Baseline \(main\) \| Feature \(topic&#124;branch\) \| Delta \|/);
  assert.match(markdown, /\| Completed journeys \| 100 \| 110 \| \+10\.00% \|/);
  assert.match(markdown, /\| Aggregate successful submit TPS \| 10\.000 TPS \| 12\.100 TPS \| \+21\.00% \|/);
  assert.match(markdown, /\| Client-observed E2E latency p99 \| 300\.000 ms \| 270\.000 ms \| -10\.00% \|/);
});

test('rejects malformed and incompatible reports with field-specific errors', () => {
  assert.throws(
    () => extractMetrics(report({ steps: [] }), 'baseline'),
    /baseline\.steps must contain at least one submit step/,
  );
  assert.throws(
    () => extractMetrics(report({ completed: -1 }), 'feature'),
    /feature\.completed must be a non-negative integer/,
  );
  assert.throws(
    () => extractMetrics(report({ version: 3 }), 'baseline'),
    /baseline\.version must be a supported txgen scenario report version \(1 or 2\)/,
  );
  assert.throws(
    () => compareReports(report(), report({ scenario: 'another-flow' })),
    /baseline scenario "private-flow" does not match feature scenario "another-flow"/,
  );
});

test('CLI reads reports and writes the Markdown output', t => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'zones-bench-compare-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const baselinePath = path.join(directory, 'baseline.json');
  const featurePath = path.join(directory, 'feature.json');
  const outputPath = path.join(directory, 'nested', 'comparison.md');
  fs.writeFileSync(baselinePath, JSON.stringify(report()));
  fs.writeFileSync(featurePath, JSON.stringify(report({ completed: 105 })));

  const result = spawnSync(process.execPath, [
    path.join(__dirname, 'zones-bench-compare.js'),
    '--baseline', baselinePath,
    '--feature', featurePath,
    '--output', outputPath,
    '--baseline-label', 'main',
    '--feature-label', 'feature',
  ], { encoding: 'utf8' });

  assert.equal(result.status, 0, result.stderr);
  assert.match(fs.readFileSync(outputPath, 'utf8'), /\| Completed journeys \| 100 \| 105 \| \+5\.00% \|/);
});
