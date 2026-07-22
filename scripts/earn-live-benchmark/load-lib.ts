export const ALLOWED_LOAD_GATES = [10, 100, 1_000] as const
export const COST_SCALE = 1_000_000n

export type LoadSchedule = {
  userIndex: number
  payoutOffsetMs: number
  earnThinkMs: number
  redeemThinkMs: number
  offrampThinkMs: number
}

export type NumericMetric = {
  min: number
  mean: number
  p50: number
  p95: number
  p99: number
  max: number
}

export function parseLoadGate(value: string | undefined): 10 | 100 | 1_000 {
  const users = Number(value ?? '10')
  if (!ALLOWED_LOAD_GATES.includes(users as 10 | 100 | 1_000)) {
    throw new Error(`EARN_LOAD_USERS must be one of ${ALLOWED_LOAD_GATES.join(', ')}`)
  }
  return users as 10 | 100 | 1_000
}

export function positiveInteger(name: string, value: string | undefined, fallback: number) {
  const parsed = Number(value ?? String(fallback))
  if (!Number.isSafeInteger(parsed) || parsed < 1) {
    throw new Error(`${name} must be a positive safe integer`)
  }
  return parsed
}

export function nonNegativeInteger(
  name: string,
  value: string | undefined,
  fallback: number,
) {
  const parsed = Number(value ?? String(fallback))
  if (!Number.isSafeInteger(parsed) || parsed < 0) {
    throw new Error(`${name} must be a non-negative safe integer`)
  }
  return parsed
}

export function bigintEnvironment(name: string, value: string | undefined, fallback: bigint) {
  let parsed: bigint
  try {
    parsed = BigInt(value ?? fallback)
  } catch {
    throw new Error(`${name} must be an integer`)
  }
  if (parsed < 0n) throw new Error(`${name} must be non-negative`)
  return parsed
}

export function createSchedule(parameters: {
  users: number
  payoutWindowMs: number
  seed: string
  earnThink: readonly [number, number]
  redeemThink: readonly [number, number]
  offrampThink: readonly [number, number]
}): LoadSchedule[] {
  const { users, payoutWindowMs, seed } = parameters
  if (!Number.isSafeInteger(users) || users < 1) throw new Error('users must be positive')
  if (!Number.isSafeInteger(payoutWindowMs) || payoutWindowMs < 1) {
    throw new Error('payoutWindowMs must be positive')
  }
  for (const [name, range] of [
    ['earnThink', parameters.earnThink],
    ['redeemThink', parameters.redeemThink],
    ['offrampThink', parameters.offrampThink],
  ] as const) {
    if (range[0] < 0 || range[1] < range[0]) throw new Error(`${name} range is invalid`)
  }

  const slotMs = payoutWindowMs / users
  return Array.from({ length: users }, (_, userIndex) => {
    const random = mulberry32(hashSeed(`${seed}:${userIndex}`))
    return {
      userIndex,
      payoutOffsetMs: Math.min(
        payoutWindowMs - 1,
        Math.floor(userIndex * slotMs + random() * slotMs),
      ),
      earnThinkMs: randomInteger(random, ...parameters.earnThink),
      redeemThinkMs: randomInteger(random, ...parameters.redeemThink),
      offrampThinkMs: randomInteger(random, ...parameters.offrampThink),
    }
  })
}

export class Semaphore {
  readonly limit: number
  #active = 0
  #waiting: Array<() => void> = []

  constructor(limit: number) {
    if (!Number.isSafeInteger(limit) || limit < 1) throw new Error('Semaphore limit must be positive')
    this.limit = limit
  }

  async run<T>(task: () => Promise<T>): Promise<T> {
    await this.acquire()
    try {
      return await task()
    } finally {
      this.release()
    }
  }

  private async acquire() {
    if (this.#active < this.limit) {
      this.#active++
      return
    }
    await new Promise<void>((resolve) => this.#waiting.push(resolve))
    this.#active++
  }

  private release() {
    this.#active--
    this.#waiting.shift()?.()
  }
}

export function metric(values: number[]): NumericMetric | null {
  if (values.length === 0) return null
  const ordered = [...values].sort((a, b) => a - b)
  const percentile = (p: number) =>
    ordered[Math.min(ordered.length - 1, Math.max(0, Math.ceil(p * ordered.length) - 1))]!
  return {
    min: ordered[0]!,
    mean: values.reduce((sum, value) => sum + value, 0) / values.length,
    p50: percentile(0.5),
    p95: percentile(0.95),
    p99: percentile(0.99),
    max: ordered.at(-1)!,
  }
}

/**
 * Allocates an indivisible receipt fee without converting the 18-decimal integer to Number.
 * The return value has COST_SCALE sub-units per fee-token base unit.
 */
export function allocateFee(fee18: bigint, numerator: number, denominator: number) {
  if (!Number.isSafeInteger(numerator) || numerator < 0) throw new Error('invalid numerator')
  if (!Number.isSafeInteger(denominator) || denominator < 1) throw new Error('invalid denominator')
  return (fee18 * COST_SCALE * BigInt(numerator)) / BigInt(denominator)
}

export function formatScaled(value: bigint, decimals = 6) {
  if (decimals < 0 || decimals > 6) throw new Error('decimals must be between zero and six')
  const negative = value < 0n
  const absolute = negative ? -value : value
  const whole = absolute / COST_SCALE
  const fraction = (absolute % COST_SCALE).toString().padStart(6, '0').slice(0, decimals)
  return `${negative ? '-' : ''}${whole}${decimals > 0 ? `.${fraction}` : ''}`
}

export function jsonStringify(value: unknown, space?: number) {
  return JSON.stringify(value, (_key, item) => (typeof item === 'bigint' ? item.toString() : item), space)
}

export function feeManagerTransferCharge(
  logs: readonly { address: string; data: string; topics?: readonly string[] }[],
  parameters: { feeToken: string; feeManager: string; transferTopic: string },
) {
  let found = false
  let total = 0n
  for (const log of logs) {
    if (log.address.toLowerCase() !== parameters.feeToken.toLowerCase()) continue
    if (log.topics?.[0]?.toLowerCase() !== parameters.transferTopic.toLowerCase()) continue
    const toTopic = log.topics?.[2]
    if (!toTopic || !/^0x[0-9a-fA-F]{64}$/.test(toTopic)) continue
    if (`0x${toTopic.slice(-40)}`.toLowerCase() !== parameters.feeManager.toLowerCase()) continue
    found = true
    total += BigInt(log.data)
  }
  return found ? total : null
}

function hashSeed(value: string) {
  let hash = 2_166_136_261
  for (let index = 0; index < value.length; index++) {
    hash ^= value.charCodeAt(index)
    hash = Math.imul(hash, 16_777_619)
  }
  return hash >>> 0
}

function mulberry32(seed: number) {
  return () => {
    seed |= 0
    seed = (seed + 0x6d2b79f5) | 0
    let value = Math.imul(seed ^ (seed >>> 15), 1 | seed)
    value = (value + Math.imul(value ^ (value >>> 7), 61 | value)) ^ value
    return ((value ^ (value >>> 14)) >>> 0) / 4_294_967_296
  }
}

function randomInteger(random: () => number, minimum: number, maximum: number) {
  return minimum + Math.floor(random() * (maximum - minimum + 1))
}
