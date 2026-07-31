'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');

const {
  BenchCommandError,
  DEFAULTS,
  USAGE,
  isBenchCommand,
  parseBenchCommand,
} = require('./zones-bench-command.js');

test('bare Derek commands use the Zones workflow defaults', () => {
  assert.deepEqual(parseBenchCommand('derek bench'), DEFAULTS);
  assert.deepEqual(parseBenchCommand('  @decofe   bench  '), DEFAULTS);
  assert.notStrictEqual(parseBenchCommand('derek bench'), DEFAULTS);
});

test('parses every workflow input plus comparison refs', () => {
  const parsed = parseBenchCommand([
    '@decofe bench',
    'preset=third-party-recipient',
    'accounts=200',
    'count=250',
    'tps=.5',
    'max-concurrent=20',
    'state-bloat-gib=10',
    'force-bloat',
    'deposit-amount=3000000',
    'activity-amount=2',
    'withdrawal-amount=1500000',
    'swap-mechanism=stablecoin-dex',
    'recipient-mode=random',
    'bootstrap-deposit-amount=12000000',
    'callback-gas-limit=9000000',
    'swap-liquidity=12000000000',
    'l1-gas-limit=29000000',
    'l1-general-gas-limit=28000000',
    'withdrawal-max-batch-gas=19000000',
    'withdrawal-max-in-flight-batches=10',
    'zone-batch-interval-blocks=100',
    'withdrawal-poll-interval-secs=4',
    'step-timeout=90s',
    'setup-settlement-timeout-secs=100',
    'drain-timeout=0',
    'seed=42',
    'baseline=main',
    'feature=dan/zones-bench',
    'run-side=feature',
  ].join(' '));

  assert.deepEqual(parsed, {
    preset: 'third-party-recipient',
    accounts: '200',
    count: '250',
    tps: '.5',
    'max-concurrent': '20',
    'state-bloat-gib': '10',
    'force-bloat': 'true',
    'deposit-amount': '3000000',
    'activity-amount': '2',
    'withdrawal-amount': '1500000',
    'swap-mechanism': 'stablecoin-dex',
    'recipient-mode': 'random',
    'bootstrap-deposit-amount': '12000000',
    'callback-gas-limit': '9000000',
    'swap-liquidity': '12000000000',
    'l1-gas-limit': '29000000',
    'l1-general-gas-limit': '28000000',
    'withdrawal-max-batch-gas': '19000000',
    'withdrawal-max-in-flight-batches': '10',
    'zone-batch-interval-blocks': '100',
    'withdrawal-poll-interval-secs': '4',
    'step-timeout': '90s',
    'setup-settlement-timeout-secs': '100',
    'drain-timeout': '0',
    seed: '42',
    baseline: 'main',
    feature: 'dan/zones-bench',
    'run-side': 'feature',
  });
});

test('supports explicit booleans, zero seeds, quotes, and multiline arguments', () => {
  const parsed = parseBenchCommand(`derek bench
    force-bloat=false
    preset="full-journey"
    seed=0
    baseline="release/v1"
  `);

  assert.equal(parsed['force-bloat'], 'false');
  assert.equal(parsed.preset, 'full-journey');
  assert.equal(parsed.seed, '0');
  assert.equal(parsed.baseline, 'release/v1');
});

test('recognizes only complete supported command prefixes', () => {
  assert.equal(isBenchCommand('derek bench'), true);
  assert.equal(isBenchCommand('@decofe bench accounts=100'), true);
  assert.equal(isBenchCommand('derek benchmark'), false);
  assert.equal(isBenchCommand('Derek bench'), false);
  assert.equal(isBenchCommand('please derek bench'), false);
});

test('rejects unknown, bare, and duplicate arguments with usage', () => {
  for (const command of [
    'derek bench mystery=1',
    'derek bench accounts',
    'derek bench accounts=100 accounts=200',
  ]) {
    assert.throws(
      () => parseBenchCommand(command),
      error => error instanceof BenchCommandError && error.message.includes(USAGE),
    );
  }
});

test('rejects malformed command and quoting syntax', () => {
  for (const command of [
    'derek benchmark',
    'bench',
    'derek bench preset="full-journey',
    'derek bench preset=full-journey\\',
  ]) {
    assert.throws(() => parseBenchCommand(command), BenchCommandError);
  }
});

test('rejects unsupported enum and boolean values', () => {
  for (const command of [
    'derek bench preset=nope',
    'derek bench state-bloat-gib=2',
    'derek bench force-bloat=yes',
    'derek bench swap-mechanism=nope',
    'derek bench recipient-mode=nope',
    'derek bench run-side=baseline',
  ]) {
    assert.throws(() => parseBenchCommand(command), BenchCommandError);
  }
});

test('rejects invalid numeric, duration, and ref values', () => {
  for (const command of [
    'derek bench accounts=0',
    'derek bench accounts=10001',
    'derek bench count=-1',
    'derek bench tps=0',
    'derek bench tps=1e3',
    'derek bench callback-gas-limit=10000001',
    'derek bench drain-timeout=86401',
    'derek bench step-timeout=0s',
    'derek bench step-timeout=10days',
    'derek bench seed=-1',
    'derek bench baseline="branch with spaces"',
    'derek bench feature=',
  ]) {
    assert.throws(() => parseBenchCommand(command), BenchCommandError);
  }
});
