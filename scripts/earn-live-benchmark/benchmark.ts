import {
  defineChain,
  erc20Abi,
  getAbiItem,
  getAddress,
  http,
  parseAbi,
  type Address,
  type TransactionReceipt,
} from 'viem'
import { getGasPrice, getTransactionCount } from 'viem/actions'
import { mnemonicToAccount } from 'viem/accounts'
import { Abis, createClient, Storage } from 'viem/tempo'
import { tempoModerato } from 'viem/tempo/chains'
import { http as zoneHttp, zoneModerato } from 'viem/tempo/zones'

const ARGO_API = 'https://dev-eu-argo-workflows.tail388b2e.ts.net'
const PLATFORM_API = 'https://dev-eu-tempo-dev-platform.tail388b2e.ts.net'
const L1_RPC = 'http://tempo-devnet-nextfork-nodes-rpc-service.tail388b2e.ts.net:8545'
const ZONE_RPC = 'http://tempo-zone-unstable-zone-unstable-rpc.tail388b2e.ts.net:8544'
const L1_CHAIN_ID = 31_318
const ZONE_ID = 2
const ZONE_CHAIN_ID = 421_700_002
const PORTAL = getAddress('0x5Ad0000000000000000000000000000000000002')
const PATHUSD = getAddress('0x20C0000000000000000000000000000000000000')
const DEFAULT_INPUT = getAddress('0x20C0000000000000000000000000000000000001')
const TEMPO_STATE = getAddress('0x1c00000000000000000000000000000000000000')
const CALLBACK_GAS = 10_000_000n
const ZONE_TRANSACTION_GAS = 10_000_000n
const MNEMONIC = 'test test test test test test test test test test test junk'

const deployment = await latestDeployment()
const zoneStatus = await currentZoneStatus()
const GATEWAY = getAddress(process.env.EARN_GATEWAY ?? deployment.gateway)
const SHARE_TOKEN = getAddress(process.env.EARN_TOKEN ?? deployment.shareToken)
const INPUT_TOKEN = getAddress(process.env.INPUT_TOKEN ?? DEFAULT_INPUT)
const JOURNEYS = Number(process.env.JOURNEYS ?? '10')
const ASSET_AMOUNT = BigInt(process.env.ASSET_AMOUNT ?? '10000000')
const FEE_BUFFER = BigInt(process.env.FEE_BUFFER ?? '10000000')

if (!Number.isSafeInteger(JOURNEYS) || JOURNEYS < 1) throw new Error('JOURNEYS must be positive')

const account = mnemonicToAccount(MNEMONIC, { addressIndex: 0 })
const l1Chain = defineChain({
  ...tempoModerato,
  id: L1_CHAIN_ID,
  name: 'Tempo nextfork',
  feeToken: PATHUSD,
  rpcUrls: { default: { http: [L1_RPC] } },
})
const zoneChain = defineChain({
  ...zoneModerato(ZONE_ID),
  id: ZONE_CHAIN_ID,
  sourceId: L1_CHAIN_ID,
  rpcUrls: { default: { http: [ZONE_RPC] } },
})
const l1 = createClient({ chain: l1Chain, pollingInterval: 250, transport: http(L1_RPC) })
const wallet = createClient({
  account,
  chain: l1Chain,
  pollingInterval: 250,
  transport: http(L1_RPC),
})
const storage = Storage.memory()
const zone = createClient({
  account,
  chain: zoneChain,
  pollingInterval: 250,
  transport: zoneHttp(ZONE_RPC, { storage, timeout: 30_000 }),
})
const gatewayAbi = parseAbi([
  'function vaultAdapter() view returns (address)',
  'function vaultAsset() view returns (address)',
  'function shareToken() view returns (address)',
  'function depositSwapperFor(address) view returns (address)',
])
const earnDepositEvent = getAbiItem({ abi: Abis.zoneGateway, name: 'EarnDeposit' })

await zone.zone.signAuthorizationToken({ zoneId: ZONE_ID, storage })

const [vaultAsset, shareToken, route, inputName, inputSymbol, vaultName] = await Promise.all([
  l1.readContract({ address: GATEWAY, abi: gatewayAbi, functionName: 'vaultAsset' }),
  l1.readContract({ address: GATEWAY, abi: gatewayAbi, functionName: 'shareToken' }),
  l1.readContract({
    address: GATEWAY,
    abi: gatewayAbi,
    functionName: 'depositSwapperFor',
    args: [INPUT_TOKEN],
  }),
  l1.readContract({ address: INPUT_TOKEN, abi: erc20Abi, functionName: 'name' }),
  l1.readContract({ address: INPUT_TOKEN, abi: erc20Abi, functionName: 'symbol' }),
  readVaultName(),
])
if (shareToken !== SHARE_TOKEN)
  throw new Error(`Gateway share token ${shareToken} != ${SHARE_TOKEN}`)
if (route === '0x0000000000000000000000000000000000000000') {
  throw new Error(`Gateway ${GATEWAY} has no deposit route for ${INPUT_TOKEN}`)
}

const setupStarted = performance.now()
const feeFunding = await wallet.zone.encryptedDepositSync({
  amount: FEE_BUFFER,
  portalAddress: PORTAL,
  recipient: account.address,
  timeout: 180_000,
  token: PATHUSD,
  zoneId: ZONE_ID,
})
await waitForZoneTempoBlock(feeFunding.receipt.blockNumber)
const setupLatencyMs = performance.now() - setupStarted

const journeys = []
for (let index = 1; index <= JOURNEYS; index++) {
  const totalStarted = performance.now()
  const [l1PathBefore, zoneInputBefore] = await Promise.all([
    balance(l1, PATHUSD),
    balance(zone, INPUT_TOKEN),
  ])
  const inputStarted = performance.now()
  const input = await wallet.zone.encryptedDepositSync({
    amount: ASSET_AMOUNT,
    portalAddress: PORTAL,
    recipient: account.address,
    timeout: 180_000,
    token: INPUT_TOKEN,
    zoneId: ZONE_ID,
  })
  await waitForZoneTempoBlock(input.receipt.blockNumber)
  const inputLatencyMs = performance.now() - inputStarted
  const [l1PathAfter, zoneInputAfter] = await Promise.all([
    balance(l1, PATHUSD),
    balance(zone, INPUT_TOKEN),
  ])

  const prepared = await l1.earn.privateDeposit.prepare({
    assetAmount: ASSET_AMOUNT,
    assetToken: INPUT_TOKEN,
    callbackGas: CALLBACK_GAS,
    fallbackRecipient: account.address,
    gateway: GATEWAY,
    recipient: account.address,
    recoveryRecipient: account.address,
    shareAmountMin: 1n,
    vaultAssetAmountMin: 1n,
  })
  const [zonePathBefore, zoneSharesBefore, maxFeePerGas, nonce] = await Promise.all([
    balance(zone, PATHUSD),
    balance(zone, SHARE_TOKEN),
    getGasPrice(zone),
    getTransactionCount(zone, { address: account.address }),
  ])
  const earnStarted = performance.now()
  const request = await zone.zone.requestWithdrawalSync({
    ...prepared,
    chain: zoneChain,
    gas: ZONE_TRANSACTION_GAS,
    maxFeePerGas,
    maxPriorityFeePerGas: 0n,
    nonce,
  })
  const zoneReceiptLatencyMs = performance.now() - earnStarted
  const settled = await l1.earn.waitForPrivateDeposit({
    actionId: prepared.actionId,
    fromBlock: prepared.fromBlock,
    gateway: GATEWAY,
    pollingInterval: 250,
    timeout: 180_000,
  })
  const l1SettlementLatencyMs = performance.now() - earnStarted
  await waitForZoneTempoBlock(settled.tempoBlockNumber)
  const [zonePathAfter, zoneSharesAfter] = await Promise.all([
    balance(zone, PATHUSD),
    waitForBalanceIncrease(SHARE_TOKEN, zoneSharesBefore),
  ])
  const earnLatencyMs = performance.now() - earnStarted
  const [depositLog] = await l1.getLogs({
    address: GATEWAY,
    args: { actionId: prepared.actionId },
    event: earnDepositEvent,
    fromBlock: prepared.fromBlock,
    strict: true,
    toBlock: 'latest',
  })
  if (!depositLog) throw new Error(`No EarnDeposit log for ${prepared.actionId}`)
  const settlementReceipt = await l1.getTransactionReceipt({ hash: depositLog.transactionHash })
  const result = {
    index,
    input: {
      latencyMs: inputLatencyMs,
      receipt: receiptMetric(input.receipt),
      userPathUsdFeeBaseUnits: (l1PathBefore - l1PathAfter).toString(),
      receivedBaseUnits: (zoneInputAfter - zoneInputBefore).toString(),
    },
    earn: {
      latencyMs: earnLatencyMs,
      zoneReceiptLatencyMs,
      l1SettlementLatencyMs,
      zoneReturnLatencyMs: earnLatencyMs - l1SettlementLatencyMs,
      zoneReceipt: receiptMetric(request.receipt),
      zonePathUsdFeeBaseUnits: (zonePathBefore - zonePathAfter).toString(),
      l1Receipt: receiptMetric(settlementReceipt),
      inputBaseUnits: settled.inputAmount.toString(),
      vaultAssetsBaseUnits: settled.vaultAssets.toString(),
      sharesBaseUnits: settled.shares.toString(),
      receivedSharesBaseUnits: (zoneSharesAfter - zoneSharesBefore).toString(),
    },
    totalLatencyMs: performance.now() - totalStarted,
  }
  journeys.push(result)
  console.error(
    `journey ${index}/${JOURNEYS}: input=${seconds(inputLatencyMs)}s earn=${seconds(earnLatencyMs)}s total=${seconds(result.totalLatencyMs)}s`,
  )
}

console.log(
  JSON.stringify(
    {
      measuredAt: new Date().toISOString(),
      deployment,
      zoneStatus,
      network: {
        l1ChainId: L1_CHAIN_ID,
        zoneId: ZONE_ID,
        zoneChainId: ZONE_CHAIN_ID,
        portal: PORTAL,
        gateway: GATEWAY,
        shareToken: SHARE_TOKEN,
      },
      venue: { inputToken: INPUT_TOKEN, inputName, inputSymbol, route, vaultAsset, vaultName },
      setup: {
        feeFundingBaseUnits: FEE_BUFFER.toString(),
        latencyMs: setupLatencyMs,
        receipt: receiptMetric(feeFunding.receipt),
      },
      parameters: { journeys: JOURNEYS, inputAssetBaseUnits: ASSET_AMOUNT.toString() },
      journeys,
      summary: summarize(journeys),
    },
    null,
    2,
  ),
)

function receiptMetric(receipt: TransactionReceipt) {
  const price = receipt.effectiveGasPrice ?? 0n
  return {
    transactionHash: receipt.transactionHash,
    blockNumber: receipt.blockNumber.toString(),
    gasUsed: receipt.gasUsed.toString(),
    effectiveGasPrice: price.toString(),
    fee18: (receipt.gasUsed * price).toString(),
  }
}

async function balance(client: typeof l1 | typeof zone, token: Address) {
  return client.readContract({
    address: token,
    abi: erc20Abi,
    functionName: 'balanceOf',
    args: [account.address],
  })
}

async function waitForZoneTempoBlock(tempoBlockNumber: bigint) {
  const deadline = Date.now() + 180_000
  while (Date.now() < deadline) {
    const current = await zone.readContract({
      address: TEMPO_STATE,
      abi: parseAbi(['function tempoBlockNumber() view returns (uint64)']),
      functionName: 'tempoBlockNumber',
    })
    if (current >= tempoBlockNumber) return
    await wait(250)
  }
  throw new Error(`Timed out waiting for Zone Tempo block ${tempoBlockNumber}`)
}

async function waitForBalanceIncrease(token: Address, before: bigint) {
  const deadline = Date.now() + 180_000
  while (Date.now() < deadline) {
    const current = await balance(zone, token)
    if (current > before) return current
    await wait(250)
  }
  throw new Error(`Timed out waiting for ${token} balance to increase`)
}

async function readVaultName() {
  const adapter = await l1.readContract({
    address: GATEWAY,
    abi: gatewayAbi,
    functionName: 'vaultAdapter',
  })
  const engine = await l1.readContract({
    address: adapter,
    abi: parseAbi(['function engine() view returns (address)']),
    functionName: 'engine',
  })
  const vault = await l1.readContract({
    address: engine,
    abi: parseAbi(['function vault() view returns (address)']),
    functionName: 'vault',
  })
  return l1.readContract({ address: vault, abi: erc20Abi, functionName: 'name' })
}

async function latestDeployment() {
  const selector = encodeURIComponent('workflows.argoproj.io/cron-workflow=zone-txgen')
  const list = await json(
    `${ARGO_API}/api/v1/workflows/argo-workflows?listOptions.labelSelector=${selector}`,
  )
  const workflows = list.items as Array<{
    metadata: { name: string; creationTimestamp: string }
    status: { phase: string }
  }>
  const running = workflows.find((workflow) => workflow.status.phase === 'Running')
  if (running) throw new Error(`${running.metadata.name} is running; retry after it finishes`)
  const latest = workflows
    .filter((workflow) => workflow.status.phase === 'Succeeded')
    .sort((a, b) => b.metadata.creationTimestamp.localeCompare(a.metadata.creationTimestamp))[0]
  if (!latest) throw new Error('No successful zone-txgen workflow found')
  const workflow = await json(`${ARGO_API}/api/v1/workflows/argo-workflows/${latest.metadata.name}`)
  const node = Object.values(workflow.status.nodes as Record<string, any>).find(
    (candidate: any) => candidate.displayName === 'deploy-earn',
  ) as any
  const parameters = Object.fromEntries(
    node.outputs.parameters.map((parameter: { name: string; value: string }) => [
      parameter.name,
      parameter.value,
    ]),
  )
  return {
    workflow: latest.metadata.name,
    completedAt: workflow.status.finishedAt as string,
    earnRevision: parameters['earn-source-revision'] as string,
    gateway: parameters['earn-gateway-address'] as string,
    shareToken: parameters['earn-token-address'] as string,
  }
}

async function currentZoneStatus() {
  const status = await json(`${PLATFORM_API}/api/zones/tempo-zone-unstable`)
  const node = Object.values(status.zone.nodeImages as Record<string, any>)[0] as any
  if (status.summary.health !== 'healthy' || status.zone.status !== 'healthy') {
    throw new Error(`tempo-zone-unstable is not healthy: ${status.zone.statusReason}`)
  }
  return {
    namespace: status.summary.namespace as string,
    image: node.image as string,
    gitSha: node.gitSha as string,
    l1GitSha: status.zone.l1Image.gitSha as string,
    hardfork: status.zone.activeHardfork.value as string,
    latestHeight: status.zone.latestHeight.value as number,
  }
}

async function json(url: string): Promise<any> {
  const response = await fetch(url)
  if (!response.ok) throw new Error(`${url} returned HTTP ${response.status}`)
  return response.json()
}

function metric(values: number[]) {
  const ordered = [...values].sort((a, b) => a - b)
  const percentile = (p: number) =>
    ordered[Math.min(ordered.length - 1, Math.ceil(p * ordered.length) - 1)]
  return {
    min: ordered[0],
    mean: values.reduce((sum, value) => sum + value, 0) / values.length,
    p50: percentile(0.5),
    p95: percentile(0.95),
    max: ordered.at(-1),
  }
}

function summarize(results: typeof journeys) {
  return {
    inputLatencyMs: metric(results.map((result) => result.input.latencyMs)),
    earnLatencyMs: metric(results.map((result) => result.earn.latencyMs)),
    totalLatencyMs: metric(results.map((result) => result.totalLatencyMs)),
    inputGasUsed: metric(results.map((result) => Number(result.input.receipt.gasUsed))),
    inputL1Fee18: metric(results.map((result) => Number(result.input.receipt.fee18))),
    earnZoneGasUsed: metric(results.map((result) => Number(result.earn.zoneReceipt.gasUsed))),
    earnL1GasUsed: metric(results.map((result) => Number(result.earn.l1Receipt.gasUsed))),
    earnL1Fee18: metric(results.map((result) => Number(result.earn.l1Receipt.fee18))),
    inputUserPathUsdFeeBaseUnits: metric(
      results.map((result) => Number(result.input.userPathUsdFeeBaseUnits)),
    ),
    earnZonePathUsdFeeBaseUnits: metric(
      results.map((result) => Number(result.earn.zonePathUsdFeeBaseUnits)),
    ),
  }
}

function seconds(milliseconds: number) {
  return (milliseconds / 1_000).toFixed(3)
}

function wait(milliseconds: number) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds))
}
