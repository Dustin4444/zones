'use strict';

const PRESETS = Object.freeze([
  'encrypted-deposit',
  'private-withdrawal',
  'rewards-redemption',
  'full-journey',
  'slippage-bounce',
  'third-party-recipient',
  'direct-lifecycle',
  'swapped-lifecycle',
  'swapped-redemption',
]);

const DEFAULTS = Object.freeze({
  preset: 'full-journey',
  accounts: '100',
  count: '100',
  tps: '20',
  'max-concurrent': '12',
  'state-bloat-gib': '1',
  'force-bloat': 'false',
  'deposit-amount': '2000000',
  'activity-amount': '1',
  'withdrawal-amount': '1000000',
  'swap-mechanism': 'direct-swap',
  'recipient-mode': 'existing',
  'bootstrap-deposit-amount': '10000000',
  'callback-gas-limit': '10000000',
  'swap-liquidity': '10000000000',
  'l1-gas-limit': '30000000',
  'l1-general-gas-limit': '30000000',
  'withdrawal-max-batch-gas': '20000000',
  'withdrawal-max-in-flight-batches': '12',
  'zone-batch-interval-blocks': '120',
  'withdrawal-poll-interval-secs': '5',
  'step-timeout': '10m',
  'setup-settlement-timeout-secs': '120',
  'drain-timeout': '300',
  seed: '',
  baseline: '',
  feature: '',
  'run-side': 'comparison',
});

const USAGE = [
  'Usage: `derek bench [key=value ...] [force-bloat]`',
  'Aliases: `@decofe bench ...`',
  `Presets: ${PRESETS.join(', ')}`,
  'Run modes: `run-side=comparison` (default) or `run-side=feature`',
  `Keys: ${Object.keys(DEFAULTS).join(', ')}`,
].join('\n');

const COMMAND_RE = /^(?:derek|@decofe)\s+bench(?:\s+|$)/;
const STATE_BLOAT_VALUES = new Set(['0', '1', '10', '100']);
const SWAP_MECHANISMS = new Set(['direct-swap', 'simple', 'stablecoin-dex']);
const RECIPIENT_MODES = new Set(['existing', 'random']);
const RUN_SIDES = new Set(['comparison', 'feature']);
const POSITIVE_INTEGER_KEYS = new Set([
  'accounts',
  'count',
  'max-concurrent',
  'deposit-amount',
  'activity-amount',
  'withdrawal-amount',
  'bootstrap-deposit-amount',
  'callback-gas-limit',
  'swap-liquidity',
  'l1-gas-limit',
  'l1-general-gas-limit',
  'withdrawal-max-batch-gas',
  'withdrawal-max-in-flight-batches',
  'zone-batch-interval-blocks',
  'withdrawal-poll-interval-secs',
  'setup-settlement-timeout-secs',
]);
const INTEGER_LIMITS = new Map([
  ['accounts', 10_000n],
  ['count', 999_999_999n],
  ['max-concurrent', 999_999_999n],
  ['callback-gas-limit', 10_000_000n],
  ['l1-gas-limit', 30_000_000n],
  ['l1-general-gas-limit', 30_000_000n],
  ['withdrawal-max-batch-gas', 20_000_000n],
  ['withdrawal-max-in-flight-batches', 10_000n],
  ['zone-batch-interval-blocks', 1_000_000n],
  ['withdrawal-poll-interval-secs', 86_400n],
  ['setup-settlement-timeout-secs', 86_400n],
  ['drain-timeout', 86_400n],
]);

class BenchCommandError extends Error {
  constructor(reason) {
    super(`${reason}\n\n${USAGE}`);
    this.name = 'BenchCommandError';
    this.reason = reason;
    this.usage = USAGE;
  }
}

function reject(reason) {
  throw new BenchCommandError(reason);
}

function isBenchCommand(text) {
  return typeof text === 'string' && COMMAND_RE.test(text.trim());
}

function tokenize(argumentText) {
  const tokens = [];
  let token = '';
  let tokenStarted = false;
  let quote = null;

  for (let index = 0; index < argumentText.length; index += 1) {
    const char = argumentText[index];

    if (quote !== null) {
      if (char === quote) {
        quote = null;
      } else if (char === '\\' && quote === '"') {
        index += 1;
        if (index >= argumentText.length) reject('Invalid syntax: trailing escape in quoted value.');
        token += argumentText[index];
      } else {
        token += char;
      }
      continue;
    }

    if (/\s/.test(char)) {
      if (tokenStarted) {
        tokens.push(token);
        token = '';
        tokenStarted = false;
      }
    } else if (char === '"' || char === "'") {
      quote = char;
      tokenStarted = true;
    } else if (char === '\\') {
      index += 1;
      if (index >= argumentText.length) reject('Invalid syntax: trailing escape.');
      token += argumentText[index];
      tokenStarted = true;
    } else {
      token += char;
      tokenStarted = true;
    }
  }

  if (quote !== null) reject(`Invalid syntax: unterminated ${quote} quote.`);
  if (tokenStarted) tokens.push(token);
  return tokens;
}

function validateUnsignedInteger(key, value, { allowZero = false, maxDigits = 18 } = {}) {
  if (!/^\d+$/.test(value)) reject(`Invalid ${key}: expected an unsigned integer.`);
  if (value.length > maxDigits) {
    reject(`Invalid ${key}: value is outside the supported unsigned-integer range.`);
  }

  const parsed = BigInt(value);
  if (!allowZero && parsed === 0n) reject(`Invalid ${key}: value must be greater than zero.`);

  const limit = INTEGER_LIMITS.get(key);
  if (limit !== undefined && parsed > limit) {
    reject(`Invalid ${key}: value cannot exceed ${limit}.`);
  }
}

function validateValue(key, value) {
  if (key === 'preset') {
    if (!PRESETS.includes(value)) reject(`Invalid preset: unsupported value ${JSON.stringify(value)}.`);
    return;
  }

  if (POSITIVE_INTEGER_KEYS.has(key)) {
    const maxDigits = ['accounts', 'count', 'max-concurrent'].includes(key) ? 9 : 18;
    validateUnsignedInteger(key, value, { maxDigits });
    return;
  }

  if (key === 'drain-timeout') {
    validateUnsignedInteger(key, value, { allowZero: true });
    return;
  }

  if (key === 'tps') {
    if (!/^(?:\d+(?:\.\d+)?|\.\d+)$/.test(value)) {
      reject('Invalid tps: expected a positive decimal number.');
    }
    const parsed = Number(value);
    if (!Number.isFinite(parsed) || parsed <= 0 || parsed > 999_999_999) {
      reject('Invalid tps: value must be greater than zero and no greater than 999999999.');
    }
    return;
  }

  if (key === 'state-bloat-gib') {
    if (!STATE_BLOAT_VALUES.has(value)) reject('Invalid state-bloat-gib: expected 0, 1, 10, or 100.');
    return;
  }

  if (key === 'force-bloat') {
    if (value !== 'true' && value !== 'false') reject('Invalid force-bloat: expected true or false.');
    return;
  }

  if (key === 'swap-mechanism') {
    if (!SWAP_MECHANISMS.has(value)) {
      reject('Invalid swap-mechanism: expected direct-swap, simple, or stablecoin-dex.');
    }
    return;
  }

  if (key === 'recipient-mode') {
    if (!RECIPIENT_MODES.has(value)) reject('Invalid recipient-mode: expected existing or random.');
    return;
  }

  if (key === 'step-timeout') {
    const match = /^([1-9]\d{0,8})(ms|s|m|h)$/.exec(value);
    if (!match) reject('Invalid step-timeout: expected a positive duration ending in ms, s, m, or h.');
    return;
  }

  if (key === 'seed') {
    if (value !== '') validateUnsignedInteger(key, value, { allowZero: true });
    return;
  }

  if (key === 'baseline' || key === 'feature') {
    if (!/^[A-Za-z0-9][A-Za-z0-9._/@+~-]*$/.test(value)) {
      reject(`Invalid ${key}: expected a non-empty git ref without whitespace.`);
    }
    return;
  }

  if (key === 'run-side') {
    if (!RUN_SIDES.has(value)) reject('Invalid run-side: expected comparison or feature.');
  }
}

function parseBenchCommand(text) {
  if (typeof text !== 'string') reject('Invalid command: expected comment text.');

  const command = text.trim();
  const prefix = COMMAND_RE.exec(command);
  if (!prefix) reject('Invalid command: expected `derek bench` or `@decofe bench`.');

  const values = { ...DEFAULTS };
  const seen = new Set();
  const argumentText = command.slice(prefix[0].length);

  for (const argument of tokenize(argumentText)) {
    let key;
    let value;

    if (argument === 'force-bloat') {
      key = 'force-bloat';
      value = 'true';
    } else {
      const equals = argument.indexOf('=');
      if (equals <= 0) {
        reject(`Invalid argument ${JSON.stringify(argument)}: expected key=value.`);
      }
      key = argument.slice(0, equals);
      value = argument.slice(equals + 1);
    }

    if (!Object.hasOwn(DEFAULTS, key)) reject(`Unknown argument ${JSON.stringify(key)}.`);
    if (seen.has(key)) reject(`Duplicate argument ${JSON.stringify(key)}.`);

    validateValue(key, value);
    seen.add(key);
    values[key] = value;
  }

  return values;
}

module.exports = {
  BenchCommandError,
  DEFAULTS,
  PRESETS,
  USAGE,
  isBenchCommand,
  parseBenchCommand,
};
