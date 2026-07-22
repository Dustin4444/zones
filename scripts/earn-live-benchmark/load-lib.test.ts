import assert from 'node:assert/strict'
import test from 'node:test'
import {
  COST_SCALE,
  Semaphore,
  allocateFee,
  createSchedule,
  formatScaled,
  feeManagerTransferCharge,
  metric,
  parseLoadGate,
} from './load-lib.ts'

test('only explicit load gates are accepted', () => {
  assert.equal(parseLoadGate(undefined), 10)
  assert.equal(parseLoadGate('1000'), 1_000)
  assert.throws(() => parseLoadGate('999'), /must be one of/)
})

test('the 1000-user schedule is deterministic, bounded, and ordered', () => {
  const parameters = {
    users: 1_000,
    payoutWindowMs: 60_000,
    seed: 'deel-2026-07-22',
    earnThink: [1_000, 10_000] as const,
    redeemThink: [2_000, 20_000] as const,
    offrampThink: [3_000, 30_000] as const,
  }
  const first = createSchedule(parameters)
  const second = createSchedule(parameters)
  assert.deepEqual(first, second)
  assert.equal(first.length, 1_000)
  assert.ok(first.every((entry) => entry.payoutOffsetMs >= 0 && entry.payoutOffsetMs < 60_000))
  assert.ok(first.every((entry, index) => index === 0 || entry.payoutOffsetMs >= first[index - 1]!.payoutOffsetMs))
  assert.ok(first.some((entry) => entry.earnThinkMs !== first[0]!.earnThinkMs))
})

test('shared receipt allocation retains bigint precision', () => {
  const allocated = allocateFee(101_384n * 600_000_001n, 1, 3)
  assert.equal(allocated, (101_384n * 600_000_001n * COST_SCALE) / 3n)
  assert.equal(formatScaled(allocated), '20276800033794.666666')
})

test('metrics include tail percentiles', () => {
  assert.deepEqual(metric([1, 2, 3, 4, 100]), {
    min: 1,
    mean: 22,
    p50: 3,
    p95: 100,
    p99: 100,
    max: 100,
  })
})

test('semaphore never exceeds its configured concurrency', async () => {
  const semaphore = new Semaphore(3)
  let active = 0
  let peak = 0
  await Promise.all(
    Array.from({ length: 20 }, () =>
      semaphore.run(async () => {
        active++
        peak = Math.max(peak, active)
        await new Promise((resolve) => setTimeout(resolve, 2))
        active--
      }),
    ),
  )
  assert.equal(peak, 3)
})

test('actual Tempo fee charge comes from fee-token transfers to FeeManager', () => {
  const feeToken = '0x20c0000000000000000000000000000000000000'
  const feeManager = '0xfeec000000000000000000000000000000000000'
  const transferTopic =
    '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef'
  const topic = (address: string) => `0x${address.slice(2).padStart(64, '0')}`
  const logs = [
    {
      address: feeToken,
      topics: [transferTopic, topic('0x1111111111111111111111111111111111111111'), topic(feeManager)],
      data: '0x127',
    },
    {
      address: feeToken,
      topics: [transferTopic, topic('0x1111111111111111111111111111111111111111'), topic('0x2222222222222222222222222222222222222222')],
      data: '0x989680',
    },
  ]
  assert.equal(
    feeManagerTransferCharge(logs, { feeToken, feeManager, transferTopic }),
    295n,
  )
})
