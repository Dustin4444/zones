import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname } from 'node:path'

import {
  defineChain,
  getAddress,
  http,
  isAddressEqual,
  parseAbi,
  type Address,
  type Hex,
} from 'viem'
import { mnemonicToAccount, privateKeyToAccount } from 'viem/accounts'
import { Abis, Actions, Addresses, createClient } from 'viem/tempo'
import { tempoModerato } from 'viem/tempo/chains'
import { zoneModerato } from 'viem/tempo/zones'

const ARGO_API =
  process.env.ARGO_API ?? 'https://dev-eu-argo-workflows.tail388b2e.ts.net'
const ARGO_NAMESPACE = process.env.ARGO_NAMESPACE ?? 'argo-workflows'
const CRON_WORKFLOW = process.env.CRON_WORKFLOW ?? 'zone-txgen'
const L1_RPC =
  process.env.L1_RPC_URL ??
  'http://tempo-devnet-nextfork-nodes-rpc-service.tail388b2e.ts.net:8545'
const ZONE_RPC =
  process.env.ZONE_PUBLIC_RPC_URL ??
  'http://tempo-zone-unstable-zone-unstable-rpc.tail388b2e.ts.net:8545'
const L1_CHAIN_ID = 31_318
const ZONE_ID = 2
const ZONE_CHAIN_ID = 421_700_002
const PATHUSD = getAddress('0x20C0000000000000000000000000000000000000')
const TEMPO_STATE = getAddress('0x1c00000000000000000000000000000000000000')
const SCENARIO_ACCOUNT = getAddress('0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266')
const TEST_MNEMONIC = 'test test test test test test test test test test test junk'
const MAX_AA_CALLS = 32
const RECOMMENDED_GAS_CEILING = 16_000_000n
const GAS_PAD_PERCENT = 120n
const REPORT_PATH = process.env.REPORT_PATH ?? '/tmp/output/policy-setup.json'

const userCount = positiveIntegerValue(
  'USER_COUNT/EARN_LOAD_USERS',
  process.env.USER_COUNT ?? process.env.EARN_LOAD_USERS,
  1_000,
)
const userIndexStart = nonnegativeIntegerValue(
  'USER_INDEX_START/EARN_LOAD_ACCOUNT_START_INDEX',
  process.env.USER_INDEX_START ?? process.env.EARN_LOAD_ACCOUNT_START_INDEX,
  1,
)
const batchSize = positiveInteger('POLICY_BATCH_SIZE', MAX_AA_CALLS)
if (batchSize > MAX_AA_CALLS) {
  throw new Error(`POLICY_BATCH_SIZE cannot exceed the Tempo transaction-pool limit ${MAX_AA_CALLS}`)
}

const adminPrivateKey = requiredPrivateKey('EARN_ACCESS_ADMIN_PRIVATE_KEY')
const admin = privateKeyToAccount(adminPrivateKey)
const mnemonic = process.env.USER_MNEMONIC ?? process.env.EARN_LOAD_MNEMONIC ?? TEST_MNEMONIC

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
const l1 = createClient({ chain: l1Chain, transport: http(L1_RPC, { timeout: 60_000 }) })
const wallet = createClient({
  account: admin,
  chain: l1Chain,
  transport: http(L1_RPC, { timeout: 60_000 }),
})
const zone = createClient({
  chain: zoneChain,
  transport: http(ZONE_RPC, { timeout: 60_000 }),
})
const gatewayAbi = parseAbi(['function shareToken() view returns (address)'])
const tempoStateAbi = parseAbi(['function tempoBlockNumber() view returns (uint64)'])

type User = { address: Address; addressIndex: number }
type AuthorizationReceipt = {
  addresses: Address[]
  batch: number
  blockNumber: string
  effectiveGasPrice: string
  estimatedGas: string
  fee18: string
  gasLimit: string
  gasUsed: string
  transactionHash: Hex
  userIndexes: number[]
}

await main().catch((error: unknown) => {
  // Deliberately avoid serializing provider/request objects: a private key is never
  // passed as RPC data, but terse failures are safer in shared Argo logs.
  const message = error instanceof Error ? error.message.split('\n')[0] : 'Unknown policy setup error'
  console.error(message)
  process.exitCode = 1
})

async function main() {
  await requireFrozenHourlyDeployment()
  const deployment = await latestSuccessfulDeployment()
  const shareToken = getAddress(deployment.shareToken)
  const gateway = getAddress(deployment.gateway)
  const expectedShareToken = optionalAddress('EXPECTED_SHARE_TOKEN')
  const expectedGateway = optionalAddress('EXPECTED_GATEWAY')
  if (expectedShareToken && !isAddressEqual(expectedShareToken, shareToken)) {
    throw new Error(`Latest EarnToken ${shareToken} does not match EXPECTED_SHARE_TOKEN ${expectedShareToken}`)
  }
  if (expectedGateway && !isAddressEqual(expectedGateway, gateway)) {
    throw new Error(`Latest gateway ${gateway} does not match EXPECTED_GATEWAY ${expectedGateway}`)
  }

  const [l1ChainId, zoneChainId, gatewayShareToken, transferPolicyId] = await Promise.all([
    l1.getChainId(),
    zone.getChainId(),
    l1.readContract({ address: gateway, abi: gatewayAbi, functionName: 'shareToken' }),
    l1.readContract({ address: shareToken, abi: Abis.tip20, functionName: 'transferPolicyId' }),
  ])
  if (l1ChainId !== L1_CHAIN_ID) throw new Error(`L1 chain ID ${l1ChainId} != ${L1_CHAIN_ID}`)
  if (zoneChainId !== ZONE_CHAIN_ID) throw new Error(`Zone chain ID ${zoneChainId} != ${ZONE_CHAIN_ID}`)
  if (!isAddressEqual(gatewayShareToken, shareToken)) {
    throw new Error(`Gateway share token ${gatewayShareToken} != discovered EarnToken ${shareToken}`)
  }

  const compound = await l1.readContract({
    address: Addresses.tip403Registry,
    abi: Abis.tip403Registry,
    functionName: 'compoundPolicyData',
    args: [transferPolicyId],
  })
  const [senderPolicyId, recipientPolicyId, mintRecipientPolicyId] = compound
  if (senderPolicyId !== 1n || recipientPolicyId !== mintRecipientPolicyId) {
    throw new Error(
      `EarnToken policy ${transferPolicyId} is not exit-safe: (${senderPolicyId},${recipientPolicyId},${mintRecipientPolicyId})`,
    )
  }
  const [policyType, policyAdmin] = await l1.readContract({
    address: Addresses.tip403Registry,
    abi: Abis.tip403Registry,
    functionName: 'policyData',
    args: [recipientPolicyId],
  })
  if (policyType !== 0) throw new Error(`Eligibility policy ${recipientPolicyId} is not a whitelist`)
  if (!isAddressEqual(policyAdmin, admin.address)) {
    throw new Error(`Configured key resolves to ${admin.address}; policy admin is ${policyAdmin}`)
  }
  const expectedPolicyId = optionalBigInt('EXPECTED_ELIGIBILITY_POLICY_ID')
  if (expectedPolicyId !== undefined && expectedPolicyId !== recipientPolicyId) {
    throw new Error(
      `Eligibility policy ${recipientPolicyId} does not match EXPECTED_ELIGIBILITY_POLICY_ID ${expectedPolicyId}`,
    )
  }

  await verifyZonePolicyShape(transferPolicyId, recipientPolicyId, policyAdmin)
  const users = deriveUsers(mnemonic, userIndexStart, userCount)
  const unique = new Set(users.map((user) => user.address.toLowerCase()))
  if (unique.size !== users.length) throw new Error('Derived user cohort contains duplicate addresses')

  const before = await mapInChunks(users, MAX_AA_CALLS, async (user) =>
    l1.readContract({
      address: Addresses.tip403Registry,
      abi: Abis.tip403Registry,
      functionName: 'isAuthorized',
      args: [recipientPolicyId, user.address],
    }),
  )
  const pendingUsers = users.filter((_, index) => !before[index])
  const feeBalance = await l1.readContract({
    address: PATHUSD,
    abi: Abis.tip20,
    functionName: 'balanceOf',
    args: [admin.address],
  })
  if (pendingUsers.length > 0 && feeBalance === 0n) {
    throw new Error(`Policy admin ${admin.address} has no PATHUSD for transaction fees`)
  }

  const receipts: AuthorizationReceipt[] = []
  const checkpoint = {
    status: 'authorizing',
    startedAt: new Date().toISOString(),
    deployment,
    network: {
      l1ChainId: L1_CHAIN_ID,
      l1Rpc: L1_RPC,
      zoneChainId: ZONE_CHAIN_ID,
      zoneId: ZONE_ID,
      zoneRpc: ZONE_RPC,
    },
    contracts: {
      gateway,
      shareToken,
      tip403Registry: getAddress(Addresses.tip403Registry),
    },
    policy: {
      administrator: admin.address,
      eligibilityPolicyId: recipientPolicyId.toString(),
      mintRecipientPolicyId: mintRecipientPolicyId.toString(),
      senderPolicyId: senderPolicyId.toString(),
      transferPolicyId: transferPolicyId.toString(),
    },
    cohort: {
      alreadyEligible: users.length - pendingUsers.length,
      mnemonicIncluded: false,
      userCount: users.length,
      userIndexStart,
      users,
    },
    authorization: {
      batchSize,
      feeToken: PATHUSD,
      initialFeeTokenBalance: feeBalance.toString(),
      receipts,
    },
  }
  writeReport(checkpoint)

  for (const [index, usersInBatch] of chunks(pendingUsers, batchSize).entries()) {
    const calls = usersInBatch.map((user) =>
      Actions.policy.modifyWhitelist.call({
        policyId: recipientPolicyId,
        address: user.address,
        allowed: true,
      }),
    )
    const estimatedGas = await wallet.estimateGas({ calls })
    const gas = (estimatedGas * GAS_PAD_PERCENT + 99n) / 100n
    if (gas > RECOMMENDED_GAS_CEILING) {
      throw new Error(
        `Batch ${index + 1} padded gas ${gas} exceeds the ${RECOMMENDED_GAS_CEILING} safety ceiling`,
      )
    }
    const receipt = await wallet.sendTransactionSync({
      calls,
      feeToken: PATHUSD,
      gas,
      maxPriorityFeePerGas: 0n,
      throwOnReceiptRevert: true,
      timeout: 180_000,
    })
    receipts.push({
      addresses: usersInBatch.map((user) => user.address),
      batch: index + 1,
      blockNumber: receipt.blockNumber.toString(),
      effectiveGasPrice: (receipt.effectiveGasPrice ?? 0n).toString(),
      estimatedGas: estimatedGas.toString(),
      fee18: (receipt.gasUsed * (receipt.effectiveGasPrice ?? 0n)).toString(),
      gasLimit: gas.toString(),
      gasUsed: receipt.gasUsed.toString(),
      transactionHash: receipt.transactionHash,
      userIndexes: usersInBatch.map((user) => user.addressIndex),
    })
    writeReport(checkpoint)
    console.error(
      `authorized batch ${index + 1}/${Math.ceil(pendingUsers.length / batchSize)}: ` +
        `${usersInBatch.length} users, tx ${receipt.transactionHash}, gas ${receipt.gasUsed}`,
    )
  }

  const l1Failures = await eligibilityFailures(l1, transferPolicyId, users)
  if (l1Failures.length > 0) {
    throw new Error(`L1 eligibility verification failed for ${l1Failures[0]}`)
  }
  const lastReceipt = receipts.at(-1)
  if (lastReceipt) await waitForZoneTempoBlock(BigInt(lastReceipt.blockNumber))
  const zoneFailures = await eligibilityFailures(zone, transferPolicyId, users, SCENARIO_ACCOUNT)
  if (zoneFailures.length > 0) {
    throw new Error(`Zone eligibility verification failed for ${zoneFailures[0]}`)
  }

  const finalFeeBalance = await l1.readContract({
    address: PATHUSD,
    abi: Abis.tip20,
    functionName: 'balanceOf',
    args: [admin.address],
  })
  const totalGasUsed = receipts.reduce((sum, receipt) => sum + BigInt(receipt.gasUsed), 0n)
  const totalFee18 = receipts.reduce((sum, receipt) => sum + BigInt(receipt.fee18), 0n)
  const report = {
    ...checkpoint,
    status: 'verified',
    completedAt: new Date().toISOString(),
    authorization: {
      ...checkpoint.authorization,
      finalFeeTokenBalance: finalFeeBalance.toString(),
      totalFee18: totalFee18.toString(),
      totalGasUsed: totalGasUsed.toString(),
    },
    verification: {
      l1: { eligibleUsers: users.length, transferPolicyId: transferPolicyId.toString() },
      zone: {
        eligibleUsers: users.length,
        tempoBlockNumber: (
          await zone.readContract({
            account: SCENARIO_ACCOUNT,
            address: TEMPO_STATE,
            abi: tempoStateAbi,
            functionName: 'tempoBlockNumber',
          })
        ).toString(),
        transferPolicyId: transferPolicyId.toString(),
      },
    },
  }
  writeReport(report)
  console.log(
    JSON.stringify(
      {
        reportPath: REPORT_PATH,
        workflow: deployment.workflow,
        gateway,
        shareToken,
        transferPolicyId: transferPolicyId.toString(),
        eligibilityPolicyId: recipientPolicyId.toString(),
        users: users.length,
        alreadyEligible: users.length - pendingUsers.length,
        authorizationTransactions: receipts.length,
        totalGasUsed: totalGasUsed.toString(),
        totalFee18: totalFee18.toString(),
      },
      null,
      2,
    ),
  )
}

async function requireFrozenHourlyDeployment() {
  const cron = await json(
    `${ARGO_API}/api/v1/cron-workflows/${ARGO_NAMESPACE}/${CRON_WORKFLOW}`,
  )
  if (cron.spec?.suspend !== true) {
    throw new Error(`${CRON_WORKFLOW} must be suspended before policy setup`)
  }
  if ((cron.status?.active ?? []).length > 0) {
    throw new Error(`${CRON_WORKFLOW} still has an active workflow; wait for it to finish`)
  }
}

async function latestSuccessfulDeployment() {
  const selector = encodeURIComponent(`workflows.argoproj.io/cron-workflow=${CRON_WORKFLOW}`)
  const list = await json(
    `${ARGO_API}/api/v1/workflows/${ARGO_NAMESPACE}?listOptions.labelSelector=${selector}`,
  )
  const workflows = list.items as Array<{
    metadata: { creationTimestamp: string; name: string }
    status: { phase: string }
  }>
  const running = workflows.find((workflow) => workflow.status.phase === 'Running')
  if (running) throw new Error(`${running.metadata.name} is still running`)
  const latest = workflows
    .filter((workflow) => workflow.status.phase === 'Succeeded')
    .sort((a, b) => b.metadata.creationTimestamp.localeCompare(a.metadata.creationTimestamp))[0]
  if (!latest) throw new Error(`No successful ${CRON_WORKFLOW} workflow found`)
  const workflow = await json(
    `${ARGO_API}/api/v1/workflows/${ARGO_NAMESPACE}/${latest.metadata.name}`,
  )
  const node = Object.values(workflow.status.nodes as Record<string, any>).find(
    (candidate: any) => candidate.displayName === 'deploy-earn',
  ) as any
  if (!node?.outputs?.parameters) throw new Error('Latest workflow has no deploy-earn outputs')
  const parameters = Object.fromEntries(
    node.outputs.parameters.map((parameter: { name: string; value: string }) => [
      parameter.name,
      parameter.value,
    ]),
  )
  return {
    completedAt: workflow.status.finishedAt as string,
    earnRevision: parameters['earn-source-revision'] as string,
    gateway: parameters['earn-gateway-address'] as string,
    shareToken: parameters['earn-token-address'] as string,
    workflow: latest.metadata.name,
  }
}

async function verifyZonePolicyShape(
  transferPolicyId: bigint,
  eligibilityPolicyId: bigint,
  policyAdmin: Address,
) {
  const [compound, simple, scenarioRecipient, scenarioMintRecipient] = await Promise.all([
    zone.readContract({
      account: SCENARIO_ACCOUNT,
      address: Addresses.tip403Registry,
      abi: Abis.tip403Registry,
      functionName: 'compoundPolicyData',
      args: [transferPolicyId],
    }),
    zone.readContract({
      account: SCENARIO_ACCOUNT,
      address: Addresses.tip403Registry,
      abi: Abis.tip403Registry,
      functionName: 'policyData',
      args: [eligibilityPolicyId],
    }),
    zone.readContract({
      account: SCENARIO_ACCOUNT,
      address: Addresses.tip403Registry,
      abi: Abis.tip403Registry,
      functionName: 'isAuthorizedRecipient',
      args: [transferPolicyId, SCENARIO_ACCOUNT],
    }),
    zone.readContract({
      account: SCENARIO_ACCOUNT,
      address: Addresses.tip403Registry,
      abi: Abis.tip403Registry,
      functionName: 'isAuthorizedMintRecipient',
      args: [transferPolicyId, SCENARIO_ACCOUNT],
    }),
  ])
  if (
    compound[0] !== 1n ||
    compound[1] !== eligibilityPolicyId ||
    compound[2] !== eligibilityPolicyId ||
    simple[0] !== 0 ||
    !isAddressEqual(simple[1], policyAdmin) ||
    !scenarioRecipient ||
    !scenarioMintRecipient
  ) {
    throw new Error('Zone has not mirrored the expected Earn TIP-403 policy')
  }
}

async function eligibilityFailures(
  client: typeof l1 | typeof zone,
  transferPolicyId: bigint,
  users: User[],
  account?: Address,
) {
  const statuses = await mapInChunks(users, MAX_AA_CALLS, async (user) => {
    const [recipient, mintRecipient] = await Promise.all([
      client.readContract({
        account,
        address: Addresses.tip403Registry,
        abi: Abis.tip403Registry,
        functionName: 'isAuthorizedRecipient',
        args: [transferPolicyId, user.address],
      }),
      client.readContract({
        account,
        address: Addresses.tip403Registry,
        abi: Abis.tip403Registry,
        functionName: 'isAuthorizedMintRecipient',
        args: [transferPolicyId, user.address],
      }),
    ])
    return recipient && mintRecipient
  })
  return users.filter((_, index) => !statuses[index]).map((user) => user.address)
}

async function waitForZoneTempoBlock(target: bigint) {
  const deadline = Date.now() + 180_000
  while (Date.now() < deadline) {
    const current = await zone.readContract({
      account: SCENARIO_ACCOUNT,
      address: TEMPO_STATE,
      abi: tempoStateAbi,
      functionName: 'tempoBlockNumber',
    })
    if (current >= target) return
    await new Promise((resolve) => setTimeout(resolve, 500))
  }
  throw new Error(`Timed out waiting for Zone to anchor Tempo block ${target}`)
}

function deriveUsers(sourceMnemonic: string, start: number, count: number): User[] {
  return Array.from({ length: count }, (_, offset) => {
    const addressIndex = start + offset
    return {
      address: mnemonicToAccount(sourceMnemonic, { addressIndex }).address,
      addressIndex,
    }
  })
}

function chunks<T>(items: T[], size: number): T[][] {
  const result: T[][] = []
  for (let index = 0; index < items.length; index += size) {
    result.push(items.slice(index, index + size))
  }
  return result
}

async function mapInChunks<T, R>(
  items: T[],
  size: number,
  map: (item: T, index: number) => Promise<R>,
) {
  const result: R[] = []
  for (let offset = 0; offset < items.length; offset += size) {
    const current = items.slice(offset, offset + size)
    result.push(...(await Promise.all(current.map((item, index) => map(item, offset + index)))))
  }
  return result
}

function writeReport(value: unknown) {
  mkdirSync(dirname(REPORT_PATH), { recursive: true })
  writeFileSync(
    REPORT_PATH,
    `${JSON.stringify(value, (_, candidate) =>
      typeof candidate === 'bigint' ? candidate.toString() : candidate,
    2)}\n`,
  )
}

async function json(url: string): Promise<any> {
  const response = await fetch(url)
  if (!response.ok) throw new Error(`${url} returned HTTP ${response.status}`)
  return response.json()
}

function requiredPrivateKey(name: string): Hex {
  const value = process.env[name]?.trim()
  if (!value) throw new Error(`${name} is required`)
  if (!/^0x[0-9a-fA-F]{64}$/.test(value)) throw new Error(`${name} is not a private key`)
  return value as Hex
}

function optionalAddress(name: string) {
  const value = process.env[name]?.trim()
  return value ? getAddress(value) : undefined
}

function optionalBigInt(name: string) {
  const value = process.env[name]?.trim()
  return value ? BigInt(value) : undefined
}

function positiveInteger(name: string, fallback: number) {
  return positiveIntegerValue(name, process.env[name], fallback)
}

function positiveIntegerValue(name: string, configured: string | undefined, fallback: number) {
  const value = Number(configured ?? fallback)
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${name} must be a positive integer`)
  return value
}

function nonnegativeIntegerValue(name: string, configured: string | undefined, fallback: number) {
  const value = Number(configured ?? fallback)
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${name} must be a nonnegative integer`)
  }
  return value
}
