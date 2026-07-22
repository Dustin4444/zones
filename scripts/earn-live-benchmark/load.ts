import { once } from 'node:events'
import { createWriteStream, mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import {
  decodeEventLog,
  defineChain,
  erc20Abi,
  getAddress,
  http,
  parseAbi,
  zeroHash,
  type Address,
  type Hex,
  type TransactionReceipt,
} from 'viem'
import { mnemonicToAccount, privateKeyToAccount } from 'viem/accounts'
import { Abis, createClient, Storage } from 'viem/tempo'
import { tempoModerato } from 'viem/tempo/chains'
import { http as zoneHttp, zoneModerato } from 'viem/tempo/zones'
import {
  COST_SCALE,
  Semaphore,
  allocateFee,
  bigintEnvironment,
  createSchedule,
  feeManagerTransferCharge,
  formatScaled,
  jsonStringify,
  metric,
  nonNegativeInteger,
  parseLoadGate,
  positiveInteger,
  type LoadSchedule,
} from './load-lib.ts'

const ARGO_API = 'https://dev-eu-argo-workflows.tail388b2e.ts.net'
const PLATFORM_API = 'https://dev-eu-tempo-dev-platform.tail388b2e.ts.net'
const DEFAULT_L1_RPC = 'http://tempo-devnet-nextfork-nodes-rpc-service.tail388b2e.ts.net:8545'
const DEFAULT_ZONE_PUBLIC_RPC =
  'http://tempo-zone-unstable-zone-unstable-rpc.tail388b2e.ts.net:8545'
const DEFAULT_ZONE_PRIVATE_RPC =
  'http://tempo-zone-unstable-zone-unstable-rpc.tail388b2e.ts.net:8544'
const L1_CHAIN_ID = 31_318
const ZONE_ID = 2
const ZONE_CHAIN_ID = 421_700_002
const PORTAL = getAddress('0x5Ad0000000000000000000000000000000000002')
const PATHUSD = getAddress('0x20C0000000000000000000000000000000000000')
const DEFAULT_INPUT = getAddress('0x20C0000000000000000000000000000000000001')
const TEMPO_STATE = getAddress('0x1c00000000000000000000000000000000000000')
const ZONE_INBOX = getAddress('0x1c00000000000000000000000000000000000001')
const FEE_MANAGER = getAddress('0xfeec000000000000000000000000000000000000')
const CALLBACK_GAS = 10_000_000n
const ZONE_TRANSACTION_GAS = 10_000_000n
const DEV_USER_MNEMONIC = 'test test test test test test test test test test test junk'
const TRANSFER_TOPIC =
  '0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef' as Hex

const portalEvents = parseAbi([
  'event BatchSubmitted(uint64 indexed withdrawalBatchIndex,uint256 indexed withdrawalQueueIndex,bytes32 nextProcessedDepositQueueHash,bytes32 nextBlockHash,bytes32 withdrawalQueueHash,uint64 lastProcessedDepositNumber)',
  'event WithdrawalProcessed(address indexed to,bytes32 indexed senderTag,address token,uint128 amount,bool callbackSuccess)',
  'event EncryptedDepositMade(bytes32 indexed newCurrentDepositQueueHash,address indexed sender,address token,uint128 netAmount,uint128 fee,uint256 keyIndex,bytes32 ephemeralPubkeyX,uint8 ephemeralPubkeyYParity,bytes ciphertext,bytes12 nonce,bytes16 tag,address bouncebackRecipient,uint64 depositNumber)',
])
const zoneInboxEvents = parseAbi([
  'event TempoAdvanced(bytes32 indexed tempoBlockHash,uint64 indexed tempoBlockNumber,uint256 depositsProcessed,bytes32 newProcessedDepositQueueHash,uint64 lastProcessedDepositNumber)',
  'event EncryptedDepositProcessed(bytes32 indexed depositHash,address indexed sender,address indexed to,address token,uint128 amount,bytes32 memo)',
])
const gatewayReadAbi = parseAbi([
  'function vaultAdapter() view returns (address)',
  'function vaultAsset() view returns (address)',
  'function shareToken() view returns (address)',
  'function depositSwapperFor(address) view returns (address)',
])
const tempoStateAbi = parseAbi(['function tempoBlockNumber() view returns (uint64)'])
const zoneOutboxReadAbi = parseAbi([
  'function calculateWithdrawalFee(uint64 gasLimit) view returns (uint128 fee)',
])

if (process.argv.includes('--help')) {
  console.log(`Usage: pnpm load:plan | pnpm load

The live runner accepts only the 10, 100, and 1000 user safety gates. A live run needs:

  EARN_LOAD_USERS=1000
  EARN_LOAD_CONFIRM=tempo-zone-unstable:1000
  END_USER_PRIVATE_KEY=<central Deel treasury key from the environment>

User signers default to the dev mnemonic at address indices 1..N. EARN_LOAD_MNEMONIC can
override that mnemonic; EARN_LOAD_FUNDER_PRIVATE_KEY overrides END_USER_PRIVATE_KEY. See
README.md for concurrency, payout-window, think-time, funding, and RPC overrides.`)
  process.exit(0)
}

type LoadConfig = ReturnType<typeof readConfig>
type Runtime = Awaited<ReturnType<typeof createRuntime>>
type CompleteJourney = Awaited<ReturnType<typeof runJourney>>
type FailedJourney = ReturnType<typeof failedJourney>
type Journey = CompleteJourney | FailedJourney
type ReceiptMetric = {
  transactionHash: Hex
  blockNumber: string
  gasUsed: string
  effectiveGasPrice: string
  fee18: string
  actualFeeTokenChargeBaseUnits: string | null
}

function readConfig() {
  const users = parseLoadGate(process.env.EARN_LOAD_USERS)
  const network = process.env.EARN_LOAD_NETWORK ?? 'tempo-zone-unstable'
  const range = (prefix: string, defaults: readonly [number, number]) => {
    const minimum = nonNegativeInteger(`${prefix}_MIN_MS`, process.env[`${prefix}_MIN_MS`], defaults[0])
    const maximum = nonNegativeInteger(`${prefix}_MAX_MS`, process.env[`${prefix}_MAX_MS`], defaults[1])
    if (maximum < minimum) throw new Error(`${prefix}_MAX_MS must be >= ${prefix}_MIN_MS`)
    return [minimum, maximum] as const
  }
  return {
    dryRun: process.argv.includes('--dry-run'),
    users,
    network,
    confirmation: process.env.EARN_LOAD_CONFIRM,
    mnemonic: process.env.EARN_LOAD_MNEMONIC ?? DEV_USER_MNEMONIC,
    funderPrivateKey:
      process.env.EARN_LOAD_FUNDER_PRIVATE_KEY ?? process.env.END_USER_PRIVATE_KEY,
    accountStartIndex: positiveInteger(
      'EARN_LOAD_ACCOUNT_START_INDEX',
      process.env.EARN_LOAD_ACCOUNT_START_INDEX,
      1,
    ),
    assetAmount: bigintEnvironment(
      'EARN_LOAD_ASSET_AMOUNT',
      process.env.EARN_LOAD_ASSET_AMOUNT,
      10_000_000n,
    ),
    userFeeBuffer: bigintEnvironment(
      'EARN_LOAD_USER_FEE_BUFFER',
      process.env.EARN_LOAD_USER_FEE_BUFFER,
      1_000_000n,
    ),
    l1FeeReserve: bigintEnvironment(
      'EARN_LOAD_L1_FEE_RESERVE',
      process.env.EARN_LOAD_L1_FEE_RESERVE,
      1_000_000n,
    ),
    payoutWindowMs: positiveInteger(
      'EARN_LOAD_PAYOUT_WINDOW_MS',
      process.env.EARN_LOAD_PAYOUT_WINDOW_MS,
      60_000,
    ),
    seed: process.env.EARN_LOAD_SEED ?? 'deel-load-v1',
    earnThink: range('EARN_LOAD_EARN_THINK', [1_000, 60_000]),
    redeemThink: range('EARN_LOAD_REDEEM_THINK', [5_000, 300_000]),
    offrampThink: range('EARN_LOAD_OFFRAMP_THINK', [1_000, 60_000]),
    l1SendConcurrency: positiveInteger(
      'EARN_LOAD_L1_SEND_CONCURRENCY',
      process.env.EARN_LOAD_L1_SEND_CONCURRENCY,
      16,
    ),
    zoneSendConcurrency: positiveInteger(
      'EARN_LOAD_ZONE_SEND_CONCURRENCY',
      process.env.EARN_LOAD_ZONE_SEND_CONCURRENCY,
      32,
    ),
    zoneAuthConcurrency: positiveInteger(
      'EARN_LOAD_ZONE_AUTH_CONCURRENCY',
      process.env.EARN_LOAD_ZONE_AUTH_CONCURRENCY,
      16,
    ),
    rpcReadConcurrency: positiveInteger(
      'EARN_LOAD_RPC_READ_CONCURRENCY',
      process.env.EARN_LOAD_RPC_READ_CONCURRENCY,
      32,
    ),
    timeoutMs: positiveInteger(
      'EARN_LOAD_TIMEOUT_MS',
      process.env.EARN_LOAD_TIMEOUT_MS,
      600_000,
    ),
    pollMs: positiveInteger('EARN_LOAD_POLL_MS', process.env.EARN_LOAD_POLL_MS, 500),
    maxFailures: positiveInteger(
      'EARN_LOAD_MAX_FAILURES',
      process.env.EARN_LOAD_MAX_FAILURES,
      Math.max(1, Math.floor(users / 100)),
    ),
    outputRoot: process.env.EARN_LOAD_OUTPUT_DIR ?? 'artifacts',
    l1Rpc: process.env.EARN_LOAD_L1_RPC ?? DEFAULT_L1_RPC,
    zonePublicRpc: process.env.EARN_LOAD_ZONE_PUBLIC_RPC ?? DEFAULT_ZONE_PUBLIC_RPC,
    zonePrivateRpc: process.env.EARN_LOAD_ZONE_PRIVATE_RPC ?? DEFAULT_ZONE_PRIVATE_RPC,
    gatewayOverride: process.env.EARN_GATEWAY,
    shareTokenOverride: process.env.EARN_TOKEN,
    inputToken: getAddress(process.env.INPUT_TOKEN ?? DEFAULT_INPUT),
  }
}

async function runLive(config: LoadConfig, schedule: LoadSchedule[]) {
  const expectedConfirmation = `${config.network}:${config.users}`
  if (config.confirmation !== expectedConfirmation) {
    throw new Error(`Set EARN_LOAD_CONFIRM=${expectedConfirmation} for this live gate`)
  }
  if (!config.mnemonic) throw new Error('EARN_LOAD_MNEMONIC is blank')
  if (!config.funderPrivateKey) {
    throw new Error(
      'END_USER_PRIVATE_KEY is missing or blank (or set EARN_LOAD_FUNDER_PRIVATE_KEY)',
    )
  }
  if (config.funderPrivateKey === '') {
    throw new Error('EARN_LOAD_FUNDER_PRIVATE_KEY is blank; unset it or provide a value')
  }

  const runtime = await createRuntime(config)
  const runId = `${new Date().toISOString().replaceAll(':', '-').replaceAll('.', '-')}-${config.users}`
  const outputDirectory = resolve(config.outputRoot, runId)
  mkdirSync(outputDirectory, { recursive: true })
  const sink = new EventSink(resolve(outputDirectory, 'events.ndjson'))
  const startedAt = new Date().toISOString()
  const startedPerformance = performance.now()
  const startL1Block = await runtime.l1.getBlockNumber({ cacheTime: 0 })
  const startZoneBlock = await runtime.zoneRead.getBlockNumber({ cacheTime: 0 })
  const breaker = new CircuitBreaker(config.maxFailures)
  const l1Events = new L1EventCollector(runtime, sink, startL1Block, config)
  const zoneEvents = new ZoneEventCollector(runtime, sink, startZoneBlock, config)
  l1Events.start()
  zoneEvents.start()

  sink.emit('run_started', {
    runId,
    startL1Block,
    startZoneBlock,
    parameters: publicConfig(config),
    deployment: runtime.deployment,
    zoneStatus: runtime.zoneStatus,
  })

  const contexts = schedule.map((entry) => createUserContext(runtime, config, entry))
  const results: Journey[] = []
  try {
    const settled = await Promise.allSettled(
      contexts.map(async (context) => {
        try {
          const result = await runJourney({
            breaker,
            config,
            context,
            l1Events,
            runtime,
            runStarted: startedPerformance,
            sink,
            zoneEvents,
          })
          results.push(result)
          const complete = results.filter((item) => item.status === 'complete').length
          const failed = results.filter((item) => item.status === 'failed').length
          if ((complete + failed) % 10 === 0 || complete + failed === config.users) {
            console.error(`complete=${complete} failed=${failed} total=${config.users}`)
          }
          return result
        } catch (error) {
          breaker.fail(error)
          const failed = failedJourney(context, error)
          results.push(failed)
          sink.emit('journey_failed', failed)
          return failed
        }
      }),
    )
    const rejected = settled.filter((item) => item.status === 'rejected')
    if (rejected.length > 0) throw rejected[0]!.reason

    const complete = results.filter((result) => result.status === 'complete')
    if (complete.length > 0) {
      await waitForBatchCoverage(runtime, complete, startL1Block, config, sink)
    }
  } finally {
    await Promise.all([l1Events.stop(), zoneEvents.stop()])
  }

  const endedAt = new Date().toISOString()
  const endL1Block = await runtime.l1.getBlockNumber({ cacheTime: 0 })
  const endZoneBlock = await runtime.zoneRead.getBlockNumber({ cacheTime: 0 })
  const complete = results.filter((result) => result.status === 'complete')
  const batchLedger = await collectBatchLedger(
    runtime,
    complete,
    startL1Block,
    endL1Block,
    config,
  )
  const ledger = [...baseCostLedger(complete), ...batchLedger]
  const summary = summarizeRun(
    results,
    ledger,
    startedPerformance,
    Number(runtime.feeTokenDecimals),
  )
  const manifest = {
    runId,
    startedAt,
    endedAt,
    deployment: runtime.deployment,
    zoneStatus: runtime.zoneStatus,
    network: {
      l1ChainId: L1_CHAIN_ID,
      zoneId: ZONE_ID,
      zoneChainId: ZONE_CHAIN_ID,
      portal: PORTAL,
      gateway: runtime.gateway,
      shareToken: runtime.shareToken,
      inputToken: config.inputToken,
      l1Rpc: config.l1Rpc,
      zonePublicRpc: config.zonePublicRpc,
      zonePrivateRpc: config.zonePrivateRpc,
    },
    venue: runtime.venue,
    parameters: publicConfig(config),
    boundaries: { startL1Block, endL1Block, startZoneBlock, endZoneBlock },
    credentialSources: {
      userAccounts: process.env.EARN_LOAD_MNEMONIC
        ? 'EARN_LOAD_MNEMONIC'
        : 'public deterministic dev mnemonic, indices 1..N',
      funder: process.env.EARN_LOAD_FUNDER_PRIVATE_KEY
        ? 'EARN_LOAD_FUNDER_PRIVATE_KEY'
        : 'END_USER_PRIVATE_KEY',
    },
    accounting: {
      batchSubmission: 'allocated per batch input: external Zone transaction or processed deposit',
      processWithdrawals: 'allocated by WithdrawalProcessed event count in its L1 receipt',
      zoneDepositIngestion: 'allocated by EncryptedDepositProcessed event count in its Zone receipt',
      callbackAndDepositEncrypted: 'included inside the processWithdrawals receipt; never double-counted',
      proof: 'submitBatch receipt included; verifier/proof mode must be interpreted from deployment metadata',
    },
  }

  writeFileSync(resolve(outputDirectory, 'manifest.json'), `${jsonStringify(manifest, 2)}\n`)
  writeFileSync(
    resolve(outputDirectory, 'schedule.json'),
    `${jsonStringify(schedule, 2)}\n`,
  )
  writeFileSync(
    resolve(outputDirectory, 'journeys.ndjson'),
    `${results
      .sort((a, b) => a.userIndex - b.userIndex)
      .map((result) => jsonStringify(result))
      .join('\n')}\n`,
  )
  writeFileSync(resolve(outputDirectory, 'latency.csv'), latencyCsv(results))
  writeFileSync(resolve(outputDirectory, 'cost-ledger.csv'), costCsv(ledger))
  writeFileSync(resolve(outputDirectory, 'summary.json'), `${jsonStringify(summary, 2)}\n`)
  sink.emit('run_finished', { summary })
  await sink.close()

  console.log(jsonStringify({ outputDirectory, summary }, 2))
  if (summary.failedUsers > 0) process.exitCode = 1
}

async function createRuntime(config: LoadConfig) {
  const [deployment, zoneStatus] = await Promise.all([latestDeployment(), currentZoneStatus(config)])
  const gateway = getAddress(config.gatewayOverride ?? deployment.gateway)
  const shareToken = getAddress(config.shareTokenOverride ?? deployment.shareToken)
  const funder = privateKeyToAccount(config.funderPrivateKey as Hex)
  const l1Chain = defineChain({
    ...tempoModerato,
    id: L1_CHAIN_ID,
    name: 'Tempo nextfork',
    feeToken: PATHUSD,
    rpcUrls: { default: { http: [config.l1Rpc] } },
  })
  const zoneChain = defineChain({
    ...zoneModerato(ZONE_ID),
    id: ZONE_CHAIN_ID,
    sourceId: L1_CHAIN_ID,
    rpcUrls: { default: { http: [config.zonePrivateRpc] } },
  })
  const l1 = createClient({ chain: l1Chain, pollingInterval: config.pollMs, transport: http(config.l1Rpc) })
  const wallet = createClient({
    account: funder,
    chain: l1Chain,
    pollingInterval: config.pollMs,
    transport: http(config.l1Rpc),
  })
  const zoneRead = createClient({
    chain: zoneChain,
    pollingInterval: config.pollMs,
    transport: http(config.zonePublicRpc),
  })
  const [vaultAsset, deployedShareToken, route, inputName, inputSymbol, feeTokenDecimals, vaultName] =
    await Promise.all([
      l1.readContract({ address: gateway, abi: gatewayReadAbi, functionName: 'vaultAsset' }),
      l1.readContract({ address: gateway, abi: gatewayReadAbi, functionName: 'shareToken' }),
      l1.readContract({
        address: gateway,
        abi: gatewayReadAbi,
        functionName: 'depositSwapperFor',
        args: [config.inputToken],
      }),
      l1.readContract({ address: config.inputToken, abi: erc20Abi, functionName: 'name' }),
      l1.readContract({ address: config.inputToken, abi: erc20Abi, functionName: 'symbol' }),
      l1.readContract({ address: PATHUSD, abi: erc20Abi, functionName: 'decimals' }),
      readVaultName(l1, gateway),
    ])
  if (deployedShareToken !== shareToken) {
    throw new Error(`Gateway share token ${deployedShareToken} != ${shareToken}`)
  }
  if (route === '0x0000000000000000000000000000000000000000') {
    throw new Error(`Gateway ${gateway} has no deposit route for ${config.inputToken}`)
  }
  const [inputBalance, pathBalance, zoneGasPrice, callbackWithdrawalFee] = await Promise.all([
    l1.readContract({
      address: config.inputToken,
      abi: erc20Abi,
      functionName: 'balanceOf',
      args: [funder.address],
    }),
    l1.readContract({ address: PATHUSD, abi: erc20Abi, functionName: 'balanceOf', args: [funder.address] }),
    zoneRead.getGasPrice(),
    zoneRead.readContract({
      address: getAddress('0x1c00000000000000000000000000000000000002'),
      abi: zoneOutboxReadAbi,
      functionName: 'calculateWithdrawalFee',
      args: [CALLBACK_GAS],
    }),
  ])
  const inputRequired = config.assetAmount * BigInt(config.users)
  const pathRequired = config.userFeeBuffer * BigInt(config.users) + config.l1FeeReserve
  if (config.inputToken === PATHUSD) {
    if (inputBalance < inputRequired + pathRequired) {
      throw new Error(
        `Funder PATHUSD balance ${inputBalance} is below required ${inputRequired + pathRequired}`,
      )
    }
  } else {
    if (inputBalance < inputRequired) {
      throw new Error(`Funder input balance ${inputBalance} is below required ${inputRequired}`)
    }
    if (pathBalance < pathRequired) {
      throw new Error(`Funder PATHUSD balance ${pathBalance} is below required ${pathRequired}`)
    }
  }
  if ((zoneGasPrice > 0n || callbackWithdrawalFee > 0n) && config.userFeeBuffer === 0n) {
    throw new Error(
      `Zone gas price is ${zoneGasPrice} and callback withdrawal fee is ${callbackWithdrawalFee}; set EARN_LOAD_USER_FEE_BUFFER before running funded users`,
    )
  }
  return {
    config,
    deployment,
    feeTokenDecimals,
    funder,
    gateway,
    l1,
    l1Chain,
    l1Send: new Semaphore(config.l1SendConcurrency),
    read: new Semaphore(config.rpcReadConcurrency),
    shareToken,
    venue: { inputName, inputSymbol, route, vaultAsset, vaultName },
    callbackWithdrawalFee,
    wallet,
    zoneAuth: new Semaphore(config.zoneAuthConcurrency),
    zoneChain,
    zoneRead,
    zoneSend: new Semaphore(config.zoneSendConcurrency),
    zoneStatus,
  }
}

function createUserContext(runtime: Runtime, config: LoadConfig, schedule: LoadSchedule) {
  const account = mnemonicToAccount(config.mnemonic!, {
    addressIndex: config.accountStartIndex + schedule.userIndex,
  })
  let zoneClientPromise: Promise<any> | undefined
  return {
    account,
    schedule,
    get zoneClient() {
      zoneClientPromise ??= runtime.zoneAuth.run(async () => {
        const storage = Storage.memory()
        const client = createClient({
          account,
          chain: runtime.zoneChain,
          pollingInterval: config.pollMs,
          transport: zoneHttp(config.zonePrivateRpc, { storage, timeout: config.timeoutMs }),
        })
        await client.zone.signAuthorizationToken({ account, zoneId: ZONE_ID, storage })
        return client
      })
      return zoneClientPromise
    },
  }
}

async function runJourney(args: {
  breaker: CircuitBreaker
  config: LoadConfig
  context: ReturnType<typeof createUserContext>
  l1Events: L1EventCollector
  runtime: Runtime
  runStarted: number
  sink: EventSink
  zoneEvents: ZoneEventCollector
}) {
  const { breaker, config, context, l1Events, runtime, runStarted, sink, zoneEvents } = args
  const { account, schedule } = context
  const userIndex = schedule.userIndex
  await waitUntil(runStarted + schedule.payoutOffsetMs, breaker.signal)
  breaker.assertOpen()
  const journeyStarted = performance.now()
  sink.emit('journey_started', { userIndex, address: account.address, schedule })

  const feeFunding =
    config.userFeeBuffer > 0n
      ? await executeEncryptedPayout({
          amount: config.userFeeBuffer,
          kind: 'fee_funding',
          recipient: account.address,
          runtime,
          sink,
          token: PATHUSD,
          userIndex,
          zoneEvents,
        })
      : undefined
  const payout = await executeEncryptedPayout({
    amount: config.assetAmount,
    kind: 'payout',
    recipient: account.address,
    runtime,
    sink,
    token: config.inputToken,
    userIndex,
    zoneEvents,
  })

  await waitWithAbort(schedule.earnThinkMs, breaker.signal)
  breaker.assertOpen()
  const client = await context.zoneClient
  const earn = await executeEarn({
    account: account.address,
    client,
    config,
    l1Events,
    runtime,
    sink,
    userIndex,
    zoneEvents,
  })

  await waitWithAbort(schedule.redeemThinkMs, breaker.signal)
  breaker.assertOpen()
  const redeem = await executeRedeem({
    account: account.address,
    client,
    config,
    l1Events,
    runtime,
    shareAmount: BigInt(earn.returnDeposit.amount),
    sink,
    userIndex,
    zoneEvents,
  })

  await waitWithAbort(schedule.offrampThinkMs, breaker.signal)
  breaker.assertOpen()
  const offramp = await executeOfframp({
    account: account.address,
    amount: BigInt(redeem.returnDeposit.amount),
    client,
    config,
    l1Events,
    runtime,
    sink,
    userIndex,
  })
  const result = {
    status: 'complete' as const,
    userIndex,
    accountIndex: config.accountStartIndex + userIndex,
    address: account.address,
    schedule,
    feeFunding,
    payout,
    earn,
    redeem,
    offramp,
    totalLatencyMs: performance.now() - journeyStarted,
  }
  sink.emit('journey_completed', {
    userIndex,
    totalLatencyMs: result.totalLatencyMs,
    payoutLatencyMs: payout.latencyMs,
    earnLatencyMs: earn.latencyMs,
    redeemLatencyMs: redeem.latencyMs,
    offrampLatencyMs: offramp.latencyMs,
  })
  return result
}

async function executeEncryptedPayout(args: {
  amount: bigint
  kind: 'fee_funding' | 'payout'
  recipient: Address
  runtime: Runtime
  sink: EventSink
  token: Address
  userIndex: number
  zoneEvents: ZoneEventCollector
}) {
  const { amount, kind, recipient, runtime, sink, token, userIndex, zoneEvents } = args
  const intentAt = performance.now()
  sink.emit(`${kind}_intent`, { userIndex, recipient, amount, token })
  const sentAt = performance.now()
  const { receipt } = await runtime.l1Send.run(() =>
    runtime.wallet.zone.encryptedDepositSync({
      account: runtime.funder,
      amount,
      chain: runtime.l1Chain,
      nonceKey: 'expiring',
      portalAddress: PORTAL,
      recipient,
      timeout: runtime.config.timeoutMs,
      token,
      zoneId: ZONE_ID,
    }),
  )
  const receiptAt = performance.now()
  const made = encryptedDepositFromReceipt(receipt)
  const processed = await zoneEvents.waitForDeposit(made.depositHash)
  const terminalAt = processed.observedAt
  const systemReceipt = await runtime.zoneRead.getTransactionReceipt({
    hash: processed.log.transactionHash,
  })
  sink.emit(`${kind}_terminal`, {
    userIndex,
    depositHash: made.depositHash,
    depositNumber: made.depositNumber,
    transactionHash: receipt.transactionHash,
    zoneTransactionHash: processed.log.transactionHash,
  })
  return {
    latencyMs: terminalAt - intentAt,
    l1ReceiptLatencyMs: receiptAt - sentAt,
    zoneIngestionLatencyMs: terminalAt - receiptAt,
    receipt: receiptMetric(receipt),
    depositHash: made.depositHash,
    depositNumber: made.depositNumber.toString(),
    depositFeeBaseUnits: made.fee.toString(),
    receivedBaseUnits: processed.log.args.amount.toString(),
    zoneIngestionReceipt: receiptMetric(systemReceipt),
    zoneIngestionEventCount: zoneEvents.depositCountForTransaction(processed.log.transactionHash),
  }
}

async function executeEarn(args: CommonOperationArgs) {
  const { account, client, config, l1Events, runtime, sink, userIndex, zoneEvents } = args
  const intentAt = performance.now()
  sink.emit('earn_intent', { userIndex, account })
  const prepared = await runtime.l1.earn.privateDeposit.prepare({
    assetAmount: config.assetAmount,
    assetToken: config.inputToken,
    callbackGas: CALLBACK_GAS,
    fallbackRecipient: account,
    gateway: runtime.gateway,
    recipient: account,
    recoveryRecipient: account,
    shareAmountMin: 1n,
    vaultAssetAmountMin: 1n,
  })
  const feeBefore = await zoneBalance(client, PATHUSD, account)
  const submittedAt = performance.now()
  const request = await runtime.zoneSend.run(() =>
    client.zone.requestWithdrawalSync({
      ...prepared,
      chain: runtime.zoneChain,
      gas: ZONE_TRANSACTION_GAS,
    }),
  )
  const zoneReceiptAt = performance.now()
  const event = await l1Events.waitFor('earnDeposit', prepared.actionId)
  const l1EventAt = event.observedAt
  const settlementReceipt = await runtime.l1.getTransactionReceipt({ hash: event.log.transactionHash })
  const returnDepositHash = event.log.args.zoneDepositHash as Hex
  const returnMade = encryptedDepositFromReceipt(settlementReceipt, returnDepositHash)
  const returned = await zoneEvents.waitForDeposit(returnDepositHash)
  const terminalAt = returned.observedAt
  const [feeAfter, systemReceipt] = await Promise.all([
    zoneBalance(client, PATHUSD, account),
    runtime.zoneRead.getTransactionReceipt({ hash: returned.log.transactionHash }),
  ])
  sink.emit('earn_terminal', {
    userIndex,
    actionId: prepared.actionId,
    requestHash: request.receipt.transactionHash,
    settlementHash: settlementReceipt.transactionHash,
    returnDepositHash,
  })
  return {
    latencyMs: terminalAt - intentAt,
    submitToTerminalLatencyMs: terminalAt - submittedAt,
    prepareLatencyMs: submittedAt - intentAt,
    zoneReceiptLatencyMs: zoneReceiptAt - submittedAt,
    l1SettlementLatencyMs: l1EventAt - submittedAt,
    zoneReturnLatencyMs: terminalAt - l1EventAt,
    actionId: prepared.actionId,
    senderTag: request.senderTag,
    zoneReceipt: receiptMetric(request.receipt),
    zonePathUsdFeeBaseUnits: (feeBefore - feeAfter).toString(),
    l1Receipt: receiptMetric(settlementReceipt),
    processWithdrawalCount: withdrawalCount(settlementReceipt),
    inputBaseUnits: event.log.args.inputAmount.toString(),
    vaultAssetsBaseUnits: event.log.args.vaultAssets.toString(),
    sharesBaseUnits: event.log.args.shares.toString(),
    returnDeposit: {
      hash: returnDepositHash,
      number: returnMade.depositNumber.toString(),
      amount: returned.log.args.amount.toString(),
      blockNumber: returned.log.blockNumber.toString(),
      receipt: receiptMetric(systemReceipt),
      eventCount: zoneEvents.depositCountForTransaction(returned.log.transactionHash),
    },
  }
}

async function executeRedeem(args: CommonOperationArgs & { shareAmount: bigint }) {
  const { account, client, config, l1Events, runtime, shareAmount, sink, userIndex, zoneEvents } = args
  const intentAt = performance.now()
  sink.emit('redeem_intent', { userIndex, account, shareAmount })
  const prepared = await runtime.l1.earn.privateRedeem.prepare({
    assetAmountMin: 1n,
    assetToken: config.inputToken,
    callbackGas: CALLBACK_GAS,
    fallbackRecipient: account,
    gateway: runtime.gateway,
    recipient: account,
    recoveryRecipient: account,
    shareAmount,
  })
  const feeBefore = await zoneBalance(client, PATHUSD, account)
  const submittedAt = performance.now()
  const request = await runtime.zoneSend.run(() =>
    client.zone.requestWithdrawalSync({
      ...prepared,
      chain: runtime.zoneChain,
      gas: ZONE_TRANSACTION_GAS,
    }),
  )
  const zoneReceiptAt = performance.now()
  const event = await l1Events.waitFor('earnRedeem', prepared.actionId)
  const l1EventAt = event.observedAt
  const settlementReceipt = await runtime.l1.getTransactionReceipt({ hash: event.log.transactionHash })
  const returnDepositHash = event.log.args.zoneDepositHash as Hex
  const returnMade = encryptedDepositFromReceipt(settlementReceipt, returnDepositHash)
  const returned = await zoneEvents.waitForDeposit(returnDepositHash)
  const terminalAt = returned.observedAt
  const [feeAfter, systemReceipt] = await Promise.all([
    zoneBalance(client, PATHUSD, account),
    runtime.zoneRead.getTransactionReceipt({ hash: returned.log.transactionHash }),
  ])
  sink.emit('redeem_terminal', {
    userIndex,
    actionId: prepared.actionId,
    requestHash: request.receipt.transactionHash,
    settlementHash: settlementReceipt.transactionHash,
    returnDepositHash,
  })
  return {
    latencyMs: terminalAt - intentAt,
    submitToTerminalLatencyMs: terminalAt - submittedAt,
    prepareLatencyMs: submittedAt - intentAt,
    zoneReceiptLatencyMs: zoneReceiptAt - submittedAt,
    l1SettlementLatencyMs: l1EventAt - submittedAt,
    zoneReturnLatencyMs: terminalAt - l1EventAt,
    actionId: prepared.actionId,
    senderTag: request.senderTag,
    zoneReceipt: receiptMetric(request.receipt),
    zonePathUsdFeeBaseUnits: (feeBefore - feeAfter).toString(),
    l1Receipt: receiptMetric(settlementReceipt),
    processWithdrawalCount: withdrawalCount(settlementReceipt),
    sharesBaseUnits: event.log.args.shares.toString(),
    vaultAssetsBaseUnits: event.log.args.vaultAssets.toString(),
    outputBaseUnits: event.log.args.outputAmount.toString(),
    returnDeposit: {
      hash: returnDepositHash,
      number: returnMade.depositNumber.toString(),
      amount: returned.log.args.amount.toString(),
      blockNumber: returned.log.blockNumber.toString(),
      receipt: receiptMetric(systemReceipt),
      eventCount: zoneEvents.depositCountForTransaction(returned.log.transactionHash),
    },
  }
}

async function executeOfframp(
  args: Omit<CommonOperationArgs, 'zoneEvents'> & { amount: bigint },
) {
  const { account, amount, client, config, l1Events, runtime, sink, userIndex } = args
  const intentAt = performance.now()
  sink.emit('offramp_intent', { userIndex, account, amount })
  const feeBefore = await zoneBalance(client, PATHUSD, account)
  const l1BalanceBefore = await l1Balance(runtime, config.inputToken, account)
  const submittedAt = performance.now()
  const request = await runtime.zoneSend.run(() =>
    client.zone.requestWithdrawalSync({
      amount,
      callbackGas: 0n,
      chain: runtime.zoneChain,
      data: '0x',
      fallbackRecipient: account,
      gas: ZONE_TRANSACTION_GAS,
      memo: zeroHash,
      to: account,
      token: config.inputToken,
    }),
  )
  const zoneReceiptAt = performance.now()
  const event = await l1Events.waitFor('withdrawal', request.senderTag)
  const l1EventAt = event.observedAt
  const [settlementReceipt, l1BalanceAfter, feeAfter] = await Promise.all([
    runtime.l1.getTransactionReceipt({ hash: event.log.transactionHash }),
    l1Balance(runtime, config.inputToken, account),
    zoneBalance(client, PATHUSD, account),
  ])
  const terminalAt = performance.now()
  sink.emit('offramp_terminal', {
    userIndex,
    requestHash: request.receipt.transactionHash,
    settlementHash: settlementReceipt.transactionHash,
  })
  return {
    latencyMs: terminalAt - intentAt,
    submitToTerminalLatencyMs: terminalAt - submittedAt,
    zoneReceiptLatencyMs: zoneReceiptAt - submittedAt,
    l1SettlementLatencyMs: l1EventAt - submittedAt,
    l1ReadLatencyMs: terminalAt - l1EventAt,
    senderTag: request.senderTag,
    zoneReceipt: receiptMetric(request.receipt),
    zonePathUsdFeeBaseUnits: (feeBefore - feeAfter).toString(),
    l1Receipt: receiptMetric(settlementReceipt),
    processWithdrawalCount: withdrawalCount(settlementReceipt),
    outputBaseUnits: (l1BalanceAfter - l1BalanceBefore).toString(),
  }
}

type CommonOperationArgs = {
  account: Address
  client: any
  config: LoadConfig
  l1Events: L1EventCollector
  runtime: Runtime
  sink: EventSink
  userIndex: number
  zoneEvents: ZoneEventCollector
}

class L1EventCollector {
  readonly batches: any[] = []
  #cache = new Map<string, any>()
  #error: unknown
  #lastBlock: bigint
  #loop?: Promise<void>
  #running = false
  #waiters = new Map<string, Array<{ reject: (error: unknown) => void; resolve: (value: any) => void }>>()

  constructor(
    private runtime: Runtime,
    private sink: EventSink,
    startBlock: bigint,
    private config: LoadConfig,
  ) {
    this.#lastBlock = startBlock - 1n
  }

  start() {
    this.#running = true
    this.#loop = this.run()
  }

  async stop() {
    this.#running = false
    await this.#loop
  }

  async waitFor(kind: 'earnDeposit' | 'earnRedeem' | 'withdrawal', key: Hex) {
    const cacheKey = `${kind}:${key.toLowerCase()}`
    const cached = this.#cache.get(cacheKey)
    if (cached) return cached
    if (this.#error) throw this.#error
    return await withTimeout<any>(
      new Promise((resolve, reject) => {
        const waiters = this.#waiters.get(cacheKey) ?? []
        waiters.push({ reject, resolve })
        this.#waiters.set(cacheKey, waiters)
      }),
      this.config.timeoutMs,
      `Timed out waiting for ${cacheKey}`,
    )
  }

  private async run() {
    try {
      while (this.#running) {
        const latest = await this.runtime.l1.getBlockNumber({ cacheTime: 0 })
        while (this.#lastBlock < latest) {
          const toBlock = minBigint(latest, this.#lastBlock + 500n)
          const logs = await this.runtime.l1.getLogs({
            address: [PORTAL, this.runtime.gateway],
            fromBlock: this.#lastBlock + 1n,
            toBlock,
          })
          const observedAt = performance.now()
          for (const log of logs) this.consume(log, observedAt)
          this.#lastBlock = toBlock
        }
        await wait(this.config.pollMs)
      }
    } catch (error) {
      this.#error = error
      for (const waiters of this.#waiters.values()) {
        for (const waiter of waiters) waiter.reject(error)
      }
      this.#waiters.clear()
    }
  }

  private consume(log: any, observedAt: number) {
    const decoded = decodeL1Log(log)
    if (!decoded) return
    const normalized = { log: { ...log, args: decoded.args, eventName: decoded.eventName }, observedAt }
    let key: string | undefined
    if (decoded.eventName === 'EarnDeposit') key = `earnDeposit:${String(decoded.args.actionId).toLowerCase()}`
    if (decoded.eventName === 'EarnRedeem') key = `earnRedeem:${String(decoded.args.actionId).toLowerCase()}`
    if (decoded.eventName === 'WithdrawalProcessed') {
      key = `withdrawal:${String(decoded.args.senderTag).toLowerCase()}`
    }
    if (decoded.eventName === 'BatchSubmitted') this.batches.push(normalized.log)
    if (key) {
      this.#cache.set(key, normalized)
      for (const waiter of this.#waiters.get(key) ?? []) waiter.resolve(normalized)
      this.#waiters.delete(key)
    }
    this.sink.emit('l1_event', {
      eventName: decoded.eventName,
      transactionHash: log.transactionHash,
      blockNumber: log.blockNumber,
    })
  }
}

class ZoneEventCollector {
  #cache = new Map<string, any>()
  #counts = new Map<string, number>()
  #error: unknown
  #lastBlock: bigint
  #loop?: Promise<void>
  #running = false
  #waiters = new Map<string, Array<{ reject: (error: unknown) => void; resolve: (value: any) => void }>>()

  constructor(
    private runtime: Runtime,
    private sink: EventSink,
    startBlock: bigint,
    private config: LoadConfig,
  ) {
    this.#lastBlock = startBlock - 1n
  }

  start() {
    this.#running = true
    this.#loop = this.run()
  }

  async stop() {
    this.#running = false
    await this.#loop
  }

  depositCountForTransaction(hash: Hex) {
    return this.#counts.get(hash.toLowerCase()) ?? 1
  }

  async waitForDeposit(hash: Hex) {
    const key = hash.toLowerCase()
    const cached = this.#cache.get(key)
    if (cached) return cached
    if (this.#error) throw this.#error
    return await withTimeout<any>(
      new Promise((resolve, reject) => {
        const waiters = this.#waiters.get(key) ?? []
        waiters.push({ reject, resolve })
        this.#waiters.set(key, waiters)
      }),
      this.config.timeoutMs,
      `Timed out waiting for Zone deposit ${hash}`,
    )
  }

  private async run() {
    try {
      while (this.#running) {
        const latest = await this.runtime.zoneRead.getBlockNumber({ cacheTime: 0 })
        while (this.#lastBlock < latest) {
          const toBlock = minBigint(latest, this.#lastBlock + 500n)
          const logs = await this.runtime.zoneRead.getLogs({
            address: ZONE_INBOX,
            fromBlock: this.#lastBlock + 1n,
            toBlock,
          })
          const observedAt = performance.now()
          for (const log of logs as any[]) {
            let decoded: any
            try {
              decoded = decodeEventLog({
                abi: zoneInboxEvents,
                data: log.data,
                topics: log.topics,
              })
            } catch {
              continue
            }
            if (decoded.eventName === 'TempoAdvanced') {
              this.#counts.set(
                String(log.transactionHash).toLowerCase(),
                Number(decoded.args.depositsProcessed),
              )
              continue
            }
            if (decoded.eventName !== 'EncryptedDepositProcessed') continue
            const parsedLog = { ...log, args: decoded.args, eventName: decoded.eventName }
            const key = String(decoded.args.depositHash).toLowerCase()
            const value = { log: parsedLog, observedAt }
            this.#cache.set(key, value)
            const transactionKey = String(log.transactionHash).toLowerCase()
            if (!this.#counts.has(transactionKey)) {
              this.#counts.set(transactionKey, 1)
            }
            for (const waiter of this.#waiters.get(key) ?? []) waiter.resolve(value)
            this.#waiters.delete(key)
            this.sink.emit('zone_deposit_processed', {
              depositHash: decoded.args.depositHash,
              transactionHash: log.transactionHash,
              blockNumber: log.blockNumber,
            })
          }
          this.#lastBlock = toBlock
        }
        await wait(this.config.pollMs)
      }
    } catch (error) {
      this.#error = error
      for (const waiters of this.#waiters.values()) {
        for (const waiter of waiters) waiter.reject(error)
      }
      this.#waiters.clear()
    }
  }
}

class EventSink {
  #sequence = 0
  #stream

  constructor(path: string) {
    this.#stream = createWriteStream(path, { flags: 'wx' })
  }

  emit(type: string, data: unknown) {
    this.#stream.write(
      `${jsonStringify({ sequence: ++this.#sequence, at: new Date().toISOString(), type, data })}\n`,
    )
  }

  async close() {
    this.#stream.end()
    await once(this.#stream, 'close')
  }
}

class CircuitBreaker {
  readonly controller = new AbortController()
  failures = 0

  constructor(readonly maximumFailures: number) {}

  get signal() {
    return this.controller.signal
  }

  assertOpen() {
    if (this.signal.aborted) throw this.signal.reason
  }

  fail(error: unknown) {
    this.failures++
    if (this.failures >= this.maximumFailures && !this.signal.aborted) {
      this.controller.abort(
        new Error(`Circuit breaker opened after ${this.failures} failures: ${errorMessage(error)}`),
      )
    }
  }
}

async function waitForBatchCoverage(
  runtime: Runtime,
  journeys: Extract<Journey, { status: 'complete' }>[],
  startL1Block: bigint,
  config: LoadConfig,
  sink: EventSink,
) {
  const maxZoneBlock = journeys.reduce(
    (maximum, journey) =>
      [
        journey.payout.zoneIngestionReceipt.blockNumber,
        journey.earn.zoneReceipt.blockNumber,
        journey.earn.returnDeposit.blockNumber,
        journey.redeem.zoneReceipt.blockNumber,
        journey.redeem.returnDeposit.blockNumber,
        journey.offramp.zoneReceipt.blockNumber,
      ].reduce((inner, value) => maxBigint(inner, BigInt(value)), maximum),
    0n,
  )
  const maxDepositNumber = journeys.reduce(
    (maximum, journey) =>
      [journey.payout.depositNumber, journey.earn.returnDeposit.number, journey.redeem.returnDeposit.number]
        .reduce((inner, value) => maxBigint(inner, BigInt(value)), maximum),
    0n,
  )
  const deadline = Date.now() + config.timeoutMs
  while (Date.now() < deadline) {
    const latest = await runtime.l1.getBlockNumber({ cacheTime: 0 })
    const logs = await runtime.l1.getLogs({
      address: PORTAL,
      event: portalEvents[0],
      fromBlock: startL1Block,
      strict: true,
      toBlock: latest,
    })
    const last = logs.at(-1)
    if (last) {
      const block = await runtime.zoneRead.getBlock({ blockHash: last.args.nextBlockHash })
      if (
        block.number >= maxZoneBlock &&
        BigInt(last.args.lastProcessedDepositNumber) >= maxDepositNumber
      ) {
        sink.emit('batch_coverage_reached', {
          batchIndex: last.args.withdrawalBatchIndex,
          zoneBlockNumber: block.number,
          lastProcessedDepositNumber: last.args.lastProcessedDepositNumber,
        })
        return
      }
    }
    await wait(config.pollMs)
  }
  throw new Error(
    `Timed out waiting for submitBatch coverage of Zone block ${maxZoneBlock} and deposit ${maxDepositNumber}`,
  )
}

async function collectBatchLedger(
  runtime: Runtime,
  journeys: Extract<Journey, { status: 'complete' }>[],
  startL1Block: bigint,
  endL1Block: bigint,
  config: LoadConfig,
) {
  if (journeys.length === 0) return [] as CostRow[]
  const logs = await runtime.l1.getLogs({
    address: PORTAL,
    event: portalEvents[0],
    fromBlock: 0n,
    strict: true,
    toBlock: endL1Block,
  })
  const ordered = [...logs].sort((a, b) =>
    Number(BigInt(a.args.withdrawalBatchIndex) - BigInt(b.args.withdrawalBatchIndex)),
  )
  const enriched = await mapConcurrent(ordered, config.rpcReadConcurrency, async (log) => {
    const block = await runtime.zoneRead.getBlock({ blockHash: log.args.nextBlockHash })
    return { log, zoneBlockNumber: block.number }
  })
  const intervals = enriched.slice(1).map((current, index) => {
    const previous = enriched[index]!
    return {
      current,
      previousDepositNumber: BigInt(previous.log.args.lastProcessedDepositNumber),
      previousZoneBlock: previous.zoneBlockNumber,
    }
  })
  const components = journeys.flatMap((journey) => [
    { userIndex: journey.userIndex, address: journey.address, useCase: 'payout', kind: 'deposit', value: BigInt(journey.payout.depositNumber) },
    { userIndex: journey.userIndex, address: journey.address, useCase: 'earn', kind: 'zone', value: BigInt(journey.earn.zoneReceipt.blockNumber) },
    { userIndex: journey.userIndex, address: journey.address, useCase: 'earn', kind: 'deposit', value: BigInt(journey.earn.returnDeposit.number) },
    { userIndex: journey.userIndex, address: journey.address, useCase: 'redeem', kind: 'zone', value: BigInt(journey.redeem.zoneReceipt.blockNumber) },
    { userIndex: journey.userIndex, address: journey.address, useCase: 'redeem', kind: 'deposit', value: BigInt(journey.redeem.returnDeposit.number) },
    { userIndex: journey.userIndex, address: journey.address, useCase: 'offramp', kind: 'zone', value: BigInt(journey.offramp.zoneReceipt.blockNumber) },
  ] as const)
  const assignments = components.map((component) => {
    const interval = intervals.find(({ current, previousDepositNumber, previousZoneBlock }) =>
      component.kind === 'deposit'
        ? component.value > previousDepositNumber &&
          component.value <= BigInt(current.log.args.lastProcessedDepositNumber)
        : component.value > previousZoneBlock && component.value <= current.zoneBlockNumber,
    )
    if (!interval) {
      throw new Error(
        `No submitBatch interval covers ${component.useCase} ${component.kind} ${component.value}`,
      )
    }
    return { component, interval }
  })
  const unique = [...new Map(assignments.map((item) => [item.interval.current.log.transactionHash, item.interval])).values()]
  const accounting = new Map<string, { denominator: number; receipt: TransactionReceipt }>()
  await mapConcurrent(unique, config.rpcReadConcurrency, async (interval) => {
    const depositCount = Number(
      BigInt(interval.current.log.args.lastProcessedDepositNumber) - interval.previousDepositNumber,
    )
    const heights: bigint[] = []
    for (let height = interval.previousZoneBlock + 1n; height <= interval.current.zoneBlockNumber; height++) {
      heights.push(height)
    }
    const externalCounts = await mapConcurrent(heights, config.rpcReadConcurrency, async (height) => {
      const block = await runtime.zoneRead.getBlock({ blockNumber: height, includeTransactions: true })
      return (block.transactions as any[]).filter(
        (transaction) =>
          typeof transaction !== 'string' &&
          String(transaction.to ?? '').toLowerCase() !== ZONE_INBOX.toLowerCase(),
      ).length
    })
    const denominator = depositCount + externalCounts.reduce((sum, count) => sum + count, 0)
    if (denominator < 1) throw new Error(`Batch ${interval.current.log.transactionHash} has no inputs`)
    const receipt = await runtime.l1.getTransactionReceipt({ hash: interval.current.log.transactionHash })
    accounting.set(interval.current.log.transactionHash.toLowerCase(), { denominator, receipt })
  })
  return assignments.map(({ component, interval }): CostRow => {
    const shared = accounting.get(interval.current.log.transactionHash.toLowerCase())!
    return costRow({
      address: component.address,
      allocationDenominator: shared.denominator,
      chain: 'l1',
      component: `submitBatch.${component.kind === 'zone' ? 'request' : 'deposit'}`,
      payer: 'sequencer',
      receipt: shared.receipt,
      useCase: component.useCase,
      userIndex: component.userIndex,
    })
  })
}

type CostRow = ReturnType<typeof costRow>

function baseCostLedger(journeys: Extract<Journey, { status: 'complete' }>[]) {
  return journeys.flatMap((journey) => {
    const rows: CostRow[] = []
    if (journey.feeFunding) {
      rows.push(
        costRow({ userIndex: journey.userIndex, address: journey.address, useCase: 'setup', component: 'feeFunding.l1', chain: 'l1', payer: 'user', receipt: journey.feeFunding.receipt }),
        costRow({ userIndex: journey.userIndex, address: journey.address, useCase: 'setup', component: 'feeFunding.zoneIngestion', chain: 'zone', payer: 'sequencer', receipt: journey.feeFunding.zoneIngestionReceipt, allocationDenominator: journey.feeFunding.zoneIngestionEventCount }),
      )
    }
    rows.push(
      costRow({ userIndex: journey.userIndex, address: journey.address, useCase: 'payout', component: 'encryptedDeposit', chain: 'l1', payer: 'user', receipt: journey.payout.receipt }),
      costRow({ userIndex: journey.userIndex, address: journey.address, useCase: 'payout', component: 'zoneDepositIngestion', chain: 'zone', payer: 'sequencer', receipt: journey.payout.zoneIngestionReceipt, allocationDenominator: journey.payout.zoneIngestionEventCount }),
      ...operationRows(journey, 'earn'),
      costRow({ userIndex: journey.userIndex, address: journey.address, useCase: 'earn', component: 'zoneReturnDepositIngestion', chain: 'zone', payer: 'sequencer', receipt: journey.earn.returnDeposit.receipt, allocationDenominator: journey.earn.returnDeposit.eventCount }),
      ...operationRows(journey, 'redeem'),
      costRow({ userIndex: journey.userIndex, address: journey.address, useCase: 'redeem', component: 'zoneReturnDepositIngestion', chain: 'zone', payer: 'sequencer', receipt: journey.redeem.returnDeposit.receipt, allocationDenominator: journey.redeem.returnDeposit.eventCount }),
      ...operationRows(journey, 'offramp'),
    )
    return rows
  })
}

function operationRows(journey: Extract<Journey, { status: 'complete' }>, useCase: 'earn' | 'redeem' | 'offramp') {
  const operation = journey[useCase]
  return [
    costRow({ userIndex: journey.userIndex, address: journey.address, useCase, component: 'zoneRequest', chain: 'zone', payer: 'user', receipt: operation.zoneReceipt, tokenChargeBaseUnits: BigInt(operation.zonePathUsdFeeBaseUnits) }),
    costRow({ userIndex: journey.userIndex, address: journey.address, useCase, component: 'processWithdrawalsIncludingCallback', chain: 'l1', payer: 'sequencer', receipt: operation.l1Receipt, allocationDenominator: operation.processWithdrawalCount }),
  ]
}

function costRow(args: {
  address: Address
  allocationDenominator?: number
  chain: 'l1' | 'zone'
  component: string
  payer: 'user' | 'sequencer'
  receipt: ReturnType<typeof receiptMetric> | TransactionReceipt
  tokenChargeBaseUnits?: bigint
  useCase: string
  userIndex: number
}) {
  const denominator = args.allocationDenominator ?? 1
  const receipt = receiptMetric(args.receipt)
  const fee18 = BigInt(receipt.fee18)
  return {
    userIndex: args.userIndex,
    address: args.address,
    useCase: args.useCase,
    component: args.component,
    chain: args.chain,
    payer: args.payer,
    transactionHash: receipt.transactionHash,
    blockNumber: receipt.blockNumber,
    gasUsed: receipt.gasUsed,
    effectiveGasPrice: receipt.effectiveGasPrice,
    fee18: receipt.fee18,
    actualFeeTokenChargeBaseUnits: receipt.actualFeeTokenChargeBaseUnits,
    allocationNumerator: 1,
    allocationDenominator: denominator,
    allocatedFee18Scaled: allocateFee(fee18, 1, denominator).toString(),
    allocatedActualFeeTokenBaseUnitsScaled:
      receipt.actualFeeTokenChargeBaseUnits === null
        ? null
        : allocateFee(
            BigInt(receipt.actualFeeTokenChargeBaseUnits),
            1,
            denominator,
          ).toString(),
    pathUsdChargeBaseUnits: (args.tokenChargeBaseUnits ?? 0n).toString(),
  }
}

function summarizeRun(
  results: Journey[],
  ledger: CostRow[],
  startedPerformance: number,
  feeTokenDecimals: number,
) {
  const complete = results.filter((result): result is Extract<Journey, { status: 'complete' }> => result.status === 'complete')
  const failed = results.filter((result) => result.status === 'failed')
  const totalAllocatedFee18Scaled = ledger.reduce(
    (sum, row) => sum + BigInt(row.allocatedFee18Scaled),
    0n,
  )
  const actualRows = ledger.filter(
    (row) => row.allocatedActualFeeTokenBaseUnitsScaled !== null,
  )
  const totalAllocatedActualFeeTokenScaled = actualRows.reduce(
    (sum, row) => sum + BigInt(row.allocatedActualFeeTokenBaseUnitsScaled!),
    0n,
  )
  const chargeScale = 10n ** BigInt(18 - feeTokenDecimals)
  const costByUseCase = Object.fromEntries(
    ['setup', 'payout', 'earn', 'redeem', 'offramp'].map((useCase) => {
      const rows = ledger.filter((row) => row.useCase === useCase)
      const allocated = rows.reduce((sum, row) => sum + BigInt(row.allocatedFee18Scaled), 0n)
      const actualRows = rows.filter(
        (row) => row.allocatedActualFeeTokenBaseUnitsScaled !== null,
      )
      const actualAllocated = actualRows.reduce(
        (sum, row) => sum + BigInt(row.allocatedActualFeeTokenBaseUnitsScaled!),
        0n,
      )
      const tokenCharges = rows.reduce((sum, row) => sum + BigInt(row.pathUsdChargeBaseUnits), 0n)
      const totalScaled = allocated + tokenCharges * chargeScale * COST_SCALE
      const actualTotalScaled = actualAllocated + tokenCharges * COST_SCALE
      return [
        useCase,
        {
          actualGasChargeBaseUnits: formatScaled(actualAllocated),
          actualGasCostPathUsd: formatBaseTokenCost(
            actualAllocated,
            feeTokenDecimals,
          ),
          formulaGasFee18: formatScaled(allocated),
          formulaGasCostPathUsd: formatTokenCost(allocated),
          pathUsdChargeBaseUnits: tokenCharges.toString(),
          actualTotalCostPathUsd: formatBaseTokenCost(
            actualTotalScaled,
            feeTokenDecimals,
          ),
          formulaTotalCostPathUsd: formatTokenCost(totalScaled),
          actualReceiptCoverage: `${actualRows.length}/${rows.length}`,
        },
      ]
    }),
  )
  const latency = (useCase: 'payout' | 'earn' | 'redeem' | 'offramp', field: string) =>
    metric(complete.map((journey) => Number((journey[useCase] as any)[field])))
  return {
    requestedUsers: results.length,
    completeUsers: complete.length,
    failedUsers: failed.length,
    durationMs: performance.now() - startedPerformance,
    latencyMs: {
      payoutE2e: latency('payout', 'latencyMs'),
      earnE2e: latency('earn', 'latencyMs'),
      earnSubmitToTerminal: latency('earn', 'submitToTerminalLatencyMs'),
      redeemE2e: latency('redeem', 'latencyMs'),
      redeemSubmitToTerminal: latency('redeem', 'submitToTerminalLatencyMs'),
      offrampE2e: latency('offramp', 'latencyMs'),
      offrampSubmitToTerminal: latency('offramp', 'submitToTerminalLatencyMs'),
      journeyE2e: metric(complete.map((journey) => journey.totalLatencyMs)),
    },
    cost: {
      actualGasChargeBaseUnits: formatScaled(totalAllocatedActualFeeTokenScaled),
      actualGasCostPathUsd: formatBaseTokenCost(
        totalAllocatedActualFeeTokenScaled,
        feeTokenDecimals,
      ),
      formulaGasFee18: formatScaled(totalAllocatedFee18Scaled),
      formulaGasCostPathUsd: formatTokenCost(totalAllocatedFee18Scaled),
      feeTokenDecimals,
      actualReceiptCoverage: `${actualRows.length}/${ledger.length}`,
      byUseCase: costByUseCase,
      ledgerRows: ledger.length,
      uniqueTransactions: new Set(ledger.map((row) => row.transactionHash)).size,
    },
    failures: failed.map((result) => ({ userIndex: result.userIndex, error: result.error })),
  }
}

function latencyCsv(results: Journey[]) {
  const header = [
    'userIndex',
    'address',
    'status',
    'payoutMs',
    'earnMs',
    'earnSubmitToTerminalMs',
    'redeemMs',
    'redeemSubmitToTerminalMs',
    'offrampMs',
    'offrampSubmitToTerminalMs',
    'journeyMs',
    'error',
  ]
  const rows = results.map((result) =>
    result.status === 'complete'
      ? [result.userIndex, result.address, result.status, result.payout.latencyMs, result.earn.latencyMs, result.earn.submitToTerminalLatencyMs, result.redeem.latencyMs, result.redeem.submitToTerminalLatencyMs, result.offramp.latencyMs, result.offramp.submitToTerminalLatencyMs, result.totalLatencyMs, '']
      : [result.userIndex, result.address, result.status, '', '', '', '', '', '', '', '', result.error],
  )
  return csv([header, ...rows])
}

function costCsv(rows: CostRow[]) {
  const columns = [
    'userIndex',
    'address',
    'useCase',
    'component',
    'chain',
    'payer',
    'transactionHash',
    'blockNumber',
    'gasUsed',
    'effectiveGasPrice',
    'fee18',
    'actualFeeTokenChargeBaseUnits',
    'allocationNumerator',
    'allocationDenominator',
    'allocatedFee18Scaled',
    'allocatedActualFeeTokenBaseUnitsScaled',
    'pathUsdChargeBaseUnits',
  ] as const
  return csv([columns, ...rows.map((row) => columns.map((column) => row[column]))])
}

function receiptMetric(receipt: TransactionReceipt | ReceiptMetric): ReceiptMetric {
  if ('fee18' in receipt) return receipt
  const effectiveGasPrice = receipt.effectiveGasPrice ?? 0n
  return {
    transactionHash: receipt.transactionHash,
    blockNumber: receipt.blockNumber.toString(),
    gasUsed: receipt.gasUsed.toString(),
    effectiveGasPrice: effectiveGasPrice.toString(),
    fee18: (receipt.gasUsed * effectiveGasPrice).toString(),
    actualFeeTokenChargeBaseUnits: actualFeeTokenCharge(receipt),
  }
}

function actualFeeTokenCharge(receipt: TransactionReceipt) {
  const charge = feeManagerTransferCharge(receipt.logs as any[], {
    feeToken: PATHUSD,
    feeManager: FEE_MANAGER,
    transferTopic: TRANSFER_TOPIC,
  })
  if (charge !== null) return charge.toString()
  return receipt.gasUsed * (receipt.effectiveGasPrice ?? 0n) === 0n ? '0' : null
}

function encryptedDepositFromReceipt(receipt: TransactionReceipt, expectedHash?: Hex) {
  for (const log of receipt.logs) {
    if (log.address.toLowerCase() !== PORTAL.toLowerCase()) continue
    try {
      const decoded = decodeEventLog({ abi: portalEvents, data: log.data, topics: log.topics }) as any
      if (decoded.eventName !== 'EncryptedDepositMade') continue
      if (
        expectedHash &&
        String(decoded.args.newCurrentDepositQueueHash).toLowerCase() !== expectedHash.toLowerCase()
      ) {
        continue
      }
      return {
        depositHash: decoded.args.newCurrentDepositQueueHash as Hex,
        depositNumber: BigInt(decoded.args.depositNumber),
        fee: BigInt(decoded.args.fee),
      }
    } catch {}
  }
  throw new Error(
    `No EncryptedDepositMade${expectedHash ? ` ${expectedHash}` : ''} in ${receipt.transactionHash}`,
  )
}

function withdrawalCount(receipt: TransactionReceipt) {
  let count = 0
  for (const log of receipt.logs) {
    if (log.address.toLowerCase() !== PORTAL.toLowerCase()) continue
    try {
      const decoded = decodeEventLog({ abi: portalEvents, data: log.data, topics: log.topics })
      if (decoded.eventName === 'WithdrawalProcessed') count++
    } catch {}
  }
  if (count < 1) throw new Error(`No WithdrawalProcessed event in ${receipt.transactionHash}`)
  return count
}

function decodeL1Log(log: any) {
  for (const abi of [Abis.zoneGateway, portalEvents] as const) {
    try {
      return decodeEventLog({ abi, data: log.data, topics: log.topics, strict: false }) as any
    } catch {}
  }
  return undefined
}

async function zoneBalance(client: any, token: Address, address: Address) {
  return (await client.readContract({
    address: token,
    abi: erc20Abi,
    functionName: 'balanceOf',
    args: [address],
  })) as bigint
}

async function l1Balance(runtime: Runtime, token: Address, address: Address) {
  return (await runtime.read.run(() =>
    runtime.l1.readContract({ address: token, abi: erc20Abi, functionName: 'balanceOf', args: [address] }),
  )) as bigint
}

async function readVaultName(l1: any, gateway: Address) {
  const adapter = await l1.readContract({ address: gateway, abi: gatewayReadAbi, functionName: 'vaultAdapter' })
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
  const list = await json(`${ARGO_API}/api/v1/workflows/argo-workflows?listOptions.labelSelector=${selector}`)
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
  if (!node?.outputs?.parameters) throw new Error(`${latest.metadata.name} has no deploy-earn outputs`)
  const parameters = Object.fromEntries(
    node.outputs.parameters.map((parameter: { name: string; value: string }) => [parameter.name, parameter.value]),
  )
  return {
    workflow: latest.metadata.name,
    completedAt: workflow.status.finishedAt as string,
    earnRevision: parameters['earn-source-revision'] as string,
    gateway: parameters['earn-gateway-address'] as string,
    shareToken: parameters['earn-token-address'] as string,
  }
}

async function currentZoneStatus(config: LoadConfig) {
  const status = await json(`${PLATFORM_API}/api/zones/${config.network}`)
  const node = Object.values(status.zone.nodeImages as Record<string, any>)[0] as any
  if (status.summary.health !== 'healthy' || status.zone.status !== 'healthy') {
    throw new Error(`${config.network} is not healthy: ${status.zone.statusReason}`)
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

function publicConfig(config: LoadConfig) {
  return {
    users: config.users,
    safetyGate: config.users,
    accountStartIndex: config.accountStartIndex,
    assetAmount: config.assetAmount,
    userFeeBuffer: config.userFeeBuffer,
    l1FeeReserve: config.l1FeeReserve,
    payoutWindowMs: config.payoutWindowMs,
    seed: config.seed,
    earnThinkMs: config.earnThink,
    redeemThinkMs: config.redeemThink,
    offrampThinkMs: config.offrampThink,
    concurrency: {
      l1Send: config.l1SendConcurrency,
      zoneSend: config.zoneSendConcurrency,
      zoneAuth: config.zoneAuthConcurrency,
      rpcRead: config.rpcReadConcurrency,
    },
    timeoutMs: config.timeoutMs,
    pollMs: config.pollMs,
    maxFailures: config.maxFailures,
  }
}

function failedJourney(context: ReturnType<typeof createUserContext>, error: unknown) {
  return {
    status: 'failed' as const,
    userIndex: context.schedule.userIndex,
    address: context.account.address,
    schedule: context.schedule,
    error: errorMessage(error),
  }
}

async function mapConcurrent<T, R>(items: T[], concurrency: number, mapper: (item: T) => Promise<R>) {
  const semaphore = new Semaphore(concurrency)
  return Promise.all(items.map((item) => semaphore.run(() => mapper(item))))
}

function csv(rows: readonly (readonly unknown[])[]) {
  return `${rows
    .map((row) =>
      row
        .map((value) => {
          const text = String(value ?? '')
          return /[",\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text
        })
        .join(','),
    )
    .join('\n')}\n`
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string) {
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error(message)), timeoutMs)
      }),
    ])
  } finally {
    if (timer) clearTimeout(timer)
  }
}

async function waitUntil(target: number, signal: AbortSignal) {
  await waitWithAbort(Math.max(0, target - performance.now()), signal)
}

async function waitWithAbort(milliseconds: number, signal: AbortSignal) {
  if (signal.aborted) throw signal.reason
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(done, milliseconds)
    signal.addEventListener('abort', aborted, { once: true })
    function done() {
      signal.removeEventListener('abort', aborted)
      resolve()
    }
    function aborted() {
      clearTimeout(timer)
      reject(signal.reason)
    }
  })
}

function wait(milliseconds: number) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds))
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.stack ?? error.message : String(error)
}

function minBigint(a: bigint, b: bigint) {
  return a < b ? a : b
}

function maxBigint(a: bigint, b: bigint) {
  return a > b ? a : b
}

function formatTokenCost(scaledFee18: bigint) {
  const decimals = 24
  const negative = scaledFee18 < 0n
  const absolute = negative ? -scaledFee18 : scaledFee18
  const divisor = 10n ** BigInt(decimals)
  const whole = absolute / divisor
  const fraction = (absolute % divisor)
    .toString()
    .padStart(decimals, '0')
    .slice(0, 12)
    .replace(/0+$/, '')
  return `${negative ? '-' : ''}${whole}${fraction ? `.${fraction}` : ''}`
}

function formatBaseTokenCost(scaledBaseUnits: bigint, tokenDecimals: number) {
  const decimals = tokenDecimals + 6
  const negative = scaledBaseUnits < 0n
  const absolute = negative ? -scaledBaseUnits : scaledBaseUnits
  const divisor = 10n ** BigInt(decimals)
  const whole = absolute / divisor
  const fraction = (absolute % divisor)
    .toString()
    .padStart(decimals, '0')
    .slice(0, 12)
    .replace(/0+$/, '')
  return `${negative ? '-' : ''}${whole}${fraction ? `.${fraction}` : ''}`
}

const config = readConfig()
const schedule = createSchedule({
  users: config.users,
  payoutWindowMs: config.payoutWindowMs,
  seed: config.seed,
  earnThink: config.earnThink,
  redeemThink: config.redeemThink,
  offrampThink: config.offrampThink,
})

if (config.dryRun) {
  console.log(
    jsonStringify(
      {
        mode: 'dry-run',
        network: config.network,
        parameters: publicConfig(config),
        schedule,
      },
      2,
    ),
  )
} else {
  await runLive(config, schedule)
}
