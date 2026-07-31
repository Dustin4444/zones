#!/usr/bin/env node

'use strict';

const fs = require('node:fs');
const path = require('node:path');

const USAGE = `Usage: node .github/scripts/zones-bench-compare.js \\
  --baseline BASELINE_REPORT.json \\
  --feature FEATURE_REPORT.json \\
  --output comparison.md \\
  [--baseline-label LABEL] [--feature-label LABEL]`;

function assertObject(value, name) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${name} must be an object`);
  }
}

function assertNonNegativeInteger(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${name} must be a non-negative integer`);
  }
}

function assertNonNegativeNumber(value, name) {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) {
    throw new Error(`${name} must be a finite non-negative number`);
  }
}

function readLatency(report, reportName) {
  const field = report.client_observed_e2e_latency == null
    ? 'total_scenario_latency'
    : 'client_observed_e2e_latency';
  const latency = report[field];
  assertObject(latency, `${reportName}.${field}`);

  for (const percentile of ['p50_ms', 'p95_ms', 'p99_ms']) {
    assertNonNegativeNumber(latency[percentile], `${reportName}.${field}.${percentile}`);
  }

  return {
    source: field,
    p50Ms: latency.p50_ms,
    p95Ms: latency.p95_ms,
    p99Ms: latency.p99_ms,
  };
}

function measuredSeconds(report, reportName) {
  if (report.completed > 0 && report.completed_scenarios_per_second > 0) {
    return report.completed / report.completed_scenarios_per_second;
  }

  assertNonNegativeNumber(report.elapsed_ms, `${reportName}.elapsed_ms`);
  if (report.elapsed_ms === 0) {
    throw new Error(`${reportName}.elapsed_ms must be greater than zero when the completed journey rate is zero`);
  }
  return report.elapsed_ms / 1_000;
}

function extractMetrics(report, reportName = 'report') {
  assertObject(report, reportName);
  if (report.version !== 1 && report.version !== 2) {
    throw new Error(`${reportName}.version must be a supported txgen scenario report version (1 or 2)`);
  }
  if (typeof report.scenario !== 'string' || report.scenario.length === 0) {
    throw new Error(`${reportName}.scenario must be a non-empty string`);
  }

  for (const field of ['completed', 'failed', 'timed_out', 'maximum_in_flight']) {
    assertNonNegativeInteger(report[field], `${reportName}.${field}`);
  }
  assertNonNegativeNumber(
    report.completed_scenarios_per_second,
    `${reportName}.completed_scenarios_per_second`,
  );

  if (!Array.isArray(report.steps)) {
    throw new Error(`${reportName}.steps must be an array`);
  }
  const submitSteps = report.steps.filter(step => step && step.kind === 'submit');
  if (submitSteps.length === 0) {
    throw new Error(`${reportName}.steps must contain at least one submit step`);
  }

  let successfulSubmits = 0;
  for (const [index, step] of submitSteps.entries()) {
    assertNonNegativeInteger(step.success, `${reportName}.submit_steps[${index}].success`);
    successfulSubmits += step.success;
    if (!Number.isSafeInteger(successfulSubmits)) {
      throw new Error(`${reportName} successful submit count exceeds the safe integer range`);
    }
  }

  const elapsedSeconds = measuredSeconds(report, reportName);
  const latency = readLatency(report, reportName);

  return {
    scenario: report.scenario,
    completed: report.completed,
    failed: report.failed,
    timedOut: report.timed_out,
    journeysPerSecond: report.completed_scenarios_per_second,
    aggregateSubmitTps: successfulSubmits / elapsedSeconds,
    maximumInFlight: report.maximum_in_flight,
    latencyP50Ms: latency.p50Ms,
    latencyP95Ms: latency.p95Ms,
    latencyP99Ms: latency.p99Ms,
    latencySource: latency.source,
  };
}

function percentDelta(baseline, feature) {
  if (baseline === 0) return null;
  return ((feature - baseline) / baseline) * 100;
}

function formatDelta(baseline, feature) {
  const delta = percentDelta(baseline, feature);
  if (delta === null) return 'n/a';
  const normalized = Object.is(delta, -0) || Math.abs(delta) < 0.005 ? 0 : delta;
  return `${normalized >= 0 ? '+' : ''}${normalized.toFixed(2)}%`;
}

function formatCount(value) {
  return String(value);
}

function formatRate(value, unit) {
  return `${value.toFixed(3)} ${unit}`;
}

function formatLatency(value) {
  return `${value.toFixed(3)} ms`;
}

function escapeTableCell(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('|', '&#124;')
    .replaceAll('\r', ' ')
    .replaceAll('\n', ' ');
}

function renderComparison(baseline, feature, options = {}) {
  if (baseline.scenario !== feature.scenario) {
    throw new Error(
      `baseline scenario ${JSON.stringify(baseline.scenario)} does not match feature scenario ${JSON.stringify(feature.scenario)}`,
    );
  }

  const baselineHeader = options.baselineLabel
    ? `Baseline (${escapeTableCell(options.baselineLabel)})`
    : 'Baseline';
  const featureHeader = options.featureLabel
    ? `Feature (${escapeTableCell(options.featureLabel)})`
    : 'Feature';
  const rows = [
    ['Completed journeys', 'completed', formatCount],
    ['Failed journeys', 'failed', formatCount],
    ['Timed-out journeys', 'timedOut', formatCount],
    ['Completed journeys/s', 'journeysPerSecond', value => formatRate(value, 'journeys/s')],
    ['Aggregate successful submit TPS', 'aggregateSubmitTps', value => formatRate(value, 'TPS')],
    ['Maximum in flight', 'maximumInFlight', formatCount],
    ['Client-observed E2E latency p50', 'latencyP50Ms', formatLatency],
    ['Client-observed E2E latency p95', 'latencyP95Ms', formatLatency],
    ['Client-observed E2E latency p99', 'latencyP99Ms', formatLatency],
  ];

  const output = [
    '# Zones benchmark comparison',
    '',
    `Scenario: \`${baseline.scenario.replaceAll('`', '\\`')}\``,
    '',
    `| Metric | ${baselineHeader} | ${featureHeader} | Delta |`,
    '| --- | ---: | ---: | ---: |',
  ];
  for (const [label, key, format] of rows) {
    output.push(
      `| ${label} | ${format(baseline[key])} | ${format(feature[key])} | ${formatDelta(baseline[key], feature[key])} |`,
    );
  }
  output.push(
    '',
    '> Delta is `(feature - baseline) / baseline`. A zero baseline is reported as `n/a`. Latency uses client-observed E2E percentiles when present and otherwise falls back to total scenario latency.',
    '',
  );
  return output.join('\n');
}

function compareReports(baselineReport, featureReport, options = {}) {
  const baseline = extractMetrics(baselineReport, 'baseline');
  const feature = extractMetrics(featureReport, 'feature');
  return renderComparison(baseline, feature, options);
}

function readJson(file, name) {
  let contents;
  try {
    contents = fs.readFileSync(file, 'utf8');
  } catch (error) {
    throw new Error(`could not read ${name} report ${file}: ${error.message}`);
  }

  try {
    return JSON.parse(contents);
  } catch (error) {
    throw new Error(`could not parse ${name} report ${file}: ${error.message}`);
  }
}

function parseArgs(argv) {
  const options = {};
  const names = new Map([
    ['--baseline', 'baseline'],
    ['--feature', 'feature'],
    ['--output', 'output'],
    ['--baseline-label', 'baselineLabel'],
    ['--feature-label', 'featureLabel'],
  ]);

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') return { help: true };
    const name = names.get(arg);
    if (!name) throw new Error(`unknown argument: ${arg}`);
    const value = argv[index + 1];
    if (value === undefined || value.startsWith('--')) {
      throw new Error(`${arg} requires a value`);
    }
    options[name] = value;
    index += 1;
  }

  for (const name of ['baseline', 'feature', 'output']) {
    if (!options[name]) throw new Error(`--${name} is required`);
  }
  return options;
}

function main(argv) {
  const options = parseArgs(argv);
  if (options.help) {
    process.stdout.write(`${USAGE}\n`);
    return;
  }

  const baseline = readJson(options.baseline, 'baseline');
  const feature = readJson(options.feature, 'feature');
  const markdown = compareReports(baseline, feature, options);
  fs.mkdirSync(path.dirname(path.resolve(options.output)), { recursive: true });
  fs.writeFileSync(options.output, markdown, 'utf8');
}

if (require.main === module) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`zones-bench-compare: ${error.message}\n\n${USAGE}\n`);
    process.exitCode = 1;
  }
}

module.exports = {
  compareReports,
  extractMetrics,
  formatDelta,
  main,
  parseArgs,
  percentDelta,
  renderComparison,
};
