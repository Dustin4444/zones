import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'

import { defineChain, getAddress, http, parseAbi } from 'viem'
import { createClient } from 'viem/tempo'
import { tempoModerato } from 'viem/tempo/chains'

import { allocateFee, metric } from './load-lib.ts'

const directory = resolve(process.argv[2] ?? '')
if (!process.argv[2]) throw new Error('Usage: pnpm reconcile <artifact-directory>')

const manifest = JSON.parse(readFileSync(resolve(directory, 'manifest.json'), 'utf8'))
const journeys = readFileSync(resolve(directory, 'journeys.ndjson'), 'utf8')
  .trim()
  .split('\n')
  .map((line) => JSON.parse(line))
const rows = parseCsv(readFileSync(resolve(directory, 'cost-ledger.csv'), 'utf8'))
const originalSummary = JSON.parse(readFileSync(resolve(directory, 'summary.json'), 'utf8'))

if (
  rows.some(
    (row) => row.useCase === 'setup' && row.component === 'submitBatch.deposit',
  )
) {
  throw new Error(
    'This artifact already includes fee-reserve submitBatch attribution; no historical reconciliation is needed',
  )
}

const l1 = createClient({
  chain: defineChain({
    ...tempoModerato,
    id: manifest.network.l1ChainId,
    feeToken: getAddress('0x20C0000000000000000000000000000000000000'),
    rpcUrls: { default: { http: [manifest.network.l1Rpc] } },
  }),
  transport: http(manifest.network.l1Rpc),
})
const [batchEvent] = parseAbi([
  'event BatchSubmitted(uint64 indexed withdrawalBatchIndex,uint256 indexed withdrawalQueueIndex,bytes32 nextProcessedDepositQueueHash,bytes32 nextBlockHash,bytes32 withdrawalQueueHash,uint64 lastProcessedDepositNumber)',
])
const batches = (
  await l1.getLogs({
    address: getAddress(manifest.network.portal),
    event: batchEvent,
    fromBlock: 0n,
    strict: true,
    toBlock: BigInt(manifest.boundaries.endL1Block),
  })
).sort((a, b) => Number(a.args.withdrawalBatchIndex - b.args.withdrawalBatchIndex))

const batchRows = new Map<string, Row>()
for (const row of rows) {
  if (row.component.startsWith('submitBatch.')) {
    batchRows.set(row.transactionHash.toLowerCase(), row)
  }
}

let priorDepositNumber = 0n
const depositIntervals = batches.map((batch) => {
  const interval = {
    batch,
    first: priorDepositNumber + 1n,
    last: batch.args.lastProcessedDepositNumber,
  }
  priorDepositNumber = interval.last
  return interval
})

const added: Row[] = []
for (const journey of journeys) {
  if (journey.status !== 'complete' || !journey.feeFunding) continue
  const depositNumber = BigInt(journey.feeFunding.depositNumber)
  const interval = depositIntervals.find(
    ({ first, last }) => depositNumber >= first && depositNumber <= last,
  )
  if (!interval) throw new Error(`No submitBatch covers fee-funding deposit ${depositNumber}`)
  const template = batchRows.get(interval.batch.transactionHash.toLowerCase())
  if (!template) {
    throw new Error(
      `Batch ${interval.batch.transactionHash} has no existing allocation row; full denominator recomputation is required`,
    )
  }
  const denominator = Number(template.allocationDenominator)
  const actual = template.actualFeeTokenChargeBaseUnits
  added.push({
    ...template,
    userIndex: String(journey.userIndex),
    address: journey.address,
    useCase: 'setup',
    component: 'submitBatch.deposit',
    allocationNumerator: '1',
    allocatedFee18Scaled: allocateFee(BigInt(template.fee18), 1, denominator).toString(),
    allocatedActualFeeTokenBaseUnitsScaled:
      actual === '' ? '' : allocateFee(BigInt(actual), 1, denominator).toString(),
    pathUsdChargeBaseUnits: '0',
  })
}
if (added.length !== journeys.length) {
  throw new Error(`Expected ${journeys.length} setup batch rows, produced ${added.length}`)
}

const corrected = [...rows, ...added]
const correctedBatchGroups = new Map<string, Row[]>()
for (const row of corrected) {
  if (!row.component.startsWith('submitBatch.')) continue
  const key = row.transactionHash.toLowerCase()
  const group = correctedBatchGroups.get(key) ?? []
  group.push(row)
  correctedBatchGroups.set(key, group)
}
for (const [transactionHash, group] of correctedBatchGroups) {
  const oldDenominators = new Set(group.map((row) => Number(row.allocationDenominator)))
  if (oldDenominators.size !== 1) {
    throw new Error(`Batch ${transactionHash} has inconsistent allocation denominators`)
  }
  const oldDenominator = [...oldDenominators][0]!
  if (oldDenominator !== group.length + 1) {
    throw new Error(
      `Batch ${transactionHash} expected one zero-gas system input: denominator ${oldDenominator}, workload inputs ${group.length}`,
    )
  }
  for (const row of group) {
    row.allocationDenominator = String(group.length)
    row.allocatedFee18Scaled = allocateFee(BigInt(row.fee18), 1, group.length).toString()
    row.allocatedActualFeeTokenBaseUnitsScaled =
      row.actualFeeTokenChargeBaseUnits === ''
        ? ''
        : allocateFee(
            BigInt(row.actualFeeTokenChargeBaseUnits),
            1,
            group.length,
          ).toString()
  }
}
corrected.sort(
  (a, b) => Number(a.userIndex) - Number(b.userIndex) || componentOrder(a) - componentOrder(b),
)
writeFileSync(resolve(directory, 'cost-ledger-corrected.csv'), encodeCsv(corrected))

const components = Object.values(
  corrected.reduce<Record<string, ComponentAccumulator>>((groups, row) => {
    const key = `${row.useCase}:${row.component}`
    const group = (groups[key] ??= {
      useCase: row.useCase,
      component: row.component,
      chain: row.chain,
      payer: row.payer,
      rows: [],
    })
    group.rows.push(row)
    return groups
  }, {}),
)
  .map(summarizeComponent)
  .sort((a, b) => useCaseOrder(a.useCase) - useCaseOrder(b.useCase) || componentOrder(a) - componentOrder(b))

const byUseCase = Object.fromEntries(
  ['setup', 'payout', 'earn', 'redeem', 'offramp'].map((useCase) => {
    const selected = corrected.filter((row) => row.useCase === useCase)
    return [useCase, summarizeCostRows(selected)]
  }),
)
const total = summarizeCostRows(corrected)
const latencyStages = summarizeLatency(journeys)
const detailed = {
  reconciledAt: new Date().toISOString(),
  source: { directory, runId: manifest.runId },
  correction: {
    addedRows: added.length,
    correctedBatchReceipts: correctedBatchGroups.size,
    reason:
      'Added the fee-funding deposit share and excluded each zero-address finalizeWithdrawalBatch system transaction from the user-input denominator.',
  },
  result: {
    requestedUsers: originalSummary.requestedUsers,
    completeUsers: originalSummary.completeUsers,
    failedUsers: originalSummary.failedUsers,
    durationMs: originalSummary.durationMs,
    latencyMs: originalSummary.latencyMs,
    latencyStages,
  },
  cost: { total, byUseCase, components },
}
writeFileSync(resolve(directory, 'detailed-summary.json'), `${JSON.stringify(detailed, null, 2)}\n`)
writeFileSync(resolve(directory, 'detailed-cost-table.md'), markdownTable(detailed))
writeFileSync(resolve(directory, 'detailed-latency-table.md'), latencyMarkdown(detailed))
console.log(JSON.stringify({ directory, addedRows: added.length, cost: detailed.cost }, null, 2))

type Row = Record<string, string>
type ComponentAccumulator = {
  useCase: string
  component: string
  chain: string
  payer: string
  rows: Row[]
}

function summarizeComponent(group: ComponentAccumulator) {
  const gas = group.rows.map((row) => Number(row.gasUsed))
  const allocatedGas = group.rows.map(
    (row) => Number(row.gasUsed) / Number(row.allocationDenominator),
  )
  const cost = summarizeCostRows(group.rows)
  return {
    useCase: group.useCase,
    component: group.component,
    label: componentLabel(group.useCase, group.component),
    chain: group.chain,
    payer: group.payer,
    allocations: group.rows.length,
    uniqueTransactions: new Set(group.rows.map((row) => row.transactionHash)).size,
    receiptGas: metric(gas),
    allocatedGas: {
      total: allocatedGas.reduce((sum, value) => sum + value, 0),
      perUser: metric(allocatedGas),
    },
    ...cost,
  }
}

function summarizeCostRows(selected: Row[]) {
  const actualRows = selected.filter((row) => row.allocatedActualFeeTokenBaseUnitsScaled !== '')
  const actualScaled = actualRows.reduce(
    (sum, row) => sum + BigInt(row.allocatedActualFeeTokenBaseUnitsScaled),
    0n,
  )
  const formulaScaled = selected.reduce(
    (sum, row) => sum + BigInt(row.allocatedFee18Scaled),
    0n,
  )
  const directChargeScaled = selected.reduce(
    (sum, row) => sum + BigInt(row.pathUsdChargeBaseUnits) * 1_000_000n,
    0n,
  )
  return {
    actualCostPathUsd: decimal(actualScaled + directChargeScaled, 12),
    formulaCostPathUsd: decimal(formulaScaled, 24),
    directPathUsdCharge: decimal(directChargeScaled, 12),
    actualReceiptCoverage: `${actualRows.length}/${selected.length}`,
  }
}

function markdownTable(value: any) {
  const lines = [
    '| Use case | On-chain leg | Chain / payer | Allocations / unique txs | Receipt gas p50 / p95 | Actual attributed cost | Avg / user |',
    '|---|---|---|---:|---:|---:|---:|',
  ]
  for (const row of value.cost.components) {
    const average = Number(row.actualCostPathUsd) / value.result.completeUsers
    lines.push(
      `| ${row.useCase} | ${row.label} | ${row.chain} / ${row.payer} | ${row.allocations} / ${row.uniqueTransactions} | ${integer(row.receiptGas?.p50 ?? 0)} / ${integer(row.receiptGas?.p95 ?? 0)} | $${row.actualCostPathUsd} | $${average.toFixed(12)} |`,
    )
  }
  lines.push(
    '',
    `Corrected all-payer total: **$${value.cost.total.actualCostPathUsd}**; ` +
      `average per completed user: **$${(Number(value.cost.total.actualCostPathUsd) / value.result.completeUsers).toFixed(12)}**.`,
    '',
  )
  return `${lines.join('\n')}\n`
}

function summarizeLatency(items: any[]) {
  const complete = items.filter((item) => item.status === 'complete')
  const definitions: Array<[string, string, (item: any) => number]> = [
    ['payout', 'Intent → private Zone RPC terminal', (item) => item.payout.latencyMs],
    ['payout', 'L1 submit → L1 receipt', (item) => item.payout.l1ReceiptLatencyMs],
    ['payout', 'L1 receipt → Zone deposit observed', (item) => item.payout.zoneIngestionLatencyMs],
    ['earn', 'Intent → private Zone RPC terminal', (item) => item.earn.latencyMs],
    ['earn', 'Zone submit → private Zone RPC terminal', (item) => item.earn.submitToTerminalLatencyMs],
    ['earn', 'Prepare before Zone submit', (item) => item.earn.prepareLatencyMs],
    ['earn', 'Zone submit → Zone receipt', (item) => item.earn.zoneReceiptLatencyMs],
    ['earn', 'Zone submit → L1 process event', (item) => item.earn.l1SettlementLatencyMs],
    ['earn', 'L1 process event → Zone return observed', (item) => Math.max(0, item.earn.zoneReturnLatencyMs)],
    ['redeem', 'Intent → private Zone RPC terminal', (item) => item.redeem.latencyMs],
    ['redeem', 'Zone submit → private Zone RPC terminal', (item) => item.redeem.submitToTerminalLatencyMs],
    ['redeem', 'Prepare before Zone submit', (item) => item.redeem.prepareLatencyMs],
    ['redeem', 'Zone submit → Zone receipt', (item) => item.redeem.zoneReceiptLatencyMs],
    ['redeem', 'Zone submit → L1 process event', (item) => item.redeem.l1SettlementLatencyMs],
    ['redeem', 'L1 process event → Zone return observed', (item) => Math.max(0, item.redeem.zoneReturnLatencyMs)],
    ['offramp', 'Intent → public L1 RPC terminal', (item) => item.offramp.latencyMs],
    ['offramp', 'Zone submit → public L1 RPC terminal', (item) => item.offramp.submitToTerminalLatencyMs],
    ['offramp', 'Zone submit → Zone receipt', (item) => item.offramp.zoneReceiptLatencyMs],
    ['offramp', 'Zone submit → L1 process event', (item) => item.offramp.l1SettlementLatencyMs],
    ['offramp', 'L1 process event → final L1 read', (item) => item.offramp.l1ReadLatencyMs],
    ['journey', 'Payout intent → final L1 read, including think time', (item) => item.totalLatencyMs],
  ]
  return definitions.map(([useCase, boundary, select]) => ({
    useCase,
    boundary,
    milliseconds: metric(complete.map(select)),
  }))
}

function latencyMarkdown(value: any) {
  const lines = [
    '| Use case | Boundary | p50 | p95 | p99 | Max |',
    '|---|---|---:|---:|---:|---:|',
  ]
  for (const row of value.result.latencyStages) {
    const timing = row.milliseconds
    lines.push(
      `| ${row.useCase} | ${row.boundary} | ${seconds(timing.p50)} | ${seconds(timing.p95)} | ${seconds(timing.p99)} | ${seconds(timing.max)} |`,
    )
  }
  return `${lines.join('\n')}\n`
}

function parseCsv(source: string): Row[] {
  const lines = source.trim().split('\n')
  const headers = splitCsvLine(lines.shift()!)
  return lines.map((line) =>
    Object.fromEntries(headers.map((header, index) => [header, splitCsvLine(line)[index] ?? ''])),
  )
}

function splitCsvLine(line: string) {
  const values: string[] = []
  let value = ''
  let quoted = false
  for (let index = 0; index < line.length; index++) {
    const character = line[index]!
    if (character === '"') {
      if (quoted && line[index + 1] === '"') value += line[++index]
      else quoted = !quoted
    } else if (character === ',' && !quoted) {
      values.push(value)
      value = ''
    } else value += character
  }
  values.push(value)
  return values
}

function encodeCsv(items: Row[]) {
  if (items.length === 0) return ''
  const headers = Object.keys(items[0]!)
  const encode = (value: string) =>
    /[",\n]/.test(value) ? `"${value.replaceAll('"', '""')}"` : value
  return `${[headers, ...items.map((item) => headers.map((header) => item[header] ?? ''))]
    .map((line) => line.map(encode).join(','))
    .join('\n')}\n`
}

function decimal(value: bigint, scale: number) {
  const negative = value < 0n
  const absolute = negative ? -value : value
  const divisor = 10n ** BigInt(scale)
  const whole = absolute / divisor
  const fraction = (absolute % divisor).toString().padStart(scale, '0').replace(/0+$/, '')
  return `${negative ? '-' : ''}${whole}${fraction ? `.${fraction}` : ''}`
}

function integer(value: number) {
  return Math.round(value).toLocaleString('en-US')
}

function seconds(milliseconds: number) {
  return `${(milliseconds / 1_000).toFixed(3)}s`
}

function useCaseOrder(value: string) {
  return ['setup', 'payout', 'earn', 'redeem', 'offramp'].indexOf(value)
}

function componentOrder(value: Pick<Row, 'useCase' | 'component'>) {
  const order = [
    'feeFunding.l1',
    'feeFunding.zoneIngestion',
    'encryptedDeposit',
    'zoneDepositIngestion',
    'zoneRequest',
    'submitBatch.request',
    'processWithdrawalsIncludingCallback',
    'zoneReturnDepositIngestion',
    'submitBatch.deposit',
  ]
  return useCaseOrder(value.useCase) * 100 + order.indexOf(value.component)
}

function componentLabel(useCase: string, component: string) {
  if (component === 'feeFunding.l1') return 'Fee-reserve encrypted deposit'
  if (component === 'feeFunding.zoneIngestion') return 'Fee-reserve Zone ingestion (system tx)'
  if (component === 'encryptedDeposit') return 'AlphaUSD payout encrypted deposit'
  if (component === 'zoneDepositIngestion') return 'Payout Zone ingestion (system tx)'
  if (component === 'zoneRequest') return 'User Zone withdrawal request'
  if (component === 'submitBatch.request') return 'Outbound submitBatch / attestation'
  if (component === 'processWithdrawalsIncludingCallback') {
    return useCase === 'offramp'
      ? 'processWithdrawals + public delivery'
      : 'processWithdrawals + callback + depositEncrypted'
  }
  if (component === 'zoneReturnDepositIngestion') return 'Returned deposit Zone ingestion (system tx)'
  if (component === 'submitBatch.deposit') {
    return useCase === 'setup'
      ? 'Fee-reserve state submitBatch / attestation'
      : useCase === 'payout'
        ? 'Payout state submitBatch / attestation'
        : 'Return state submitBatch / attestation'
  }
  return component
}
