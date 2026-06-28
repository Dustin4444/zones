# Escape Hatch (Forced Exit) for Tempo Zones — Discussion Draft

If a tempo zone (which is a single operator/sequecer) ever goes offline, we need a way for the users to exit their funds from the zone directly to L1 without relying on the sequencer.  


This proposal **is**:

- A way for each user to individually recover their own TIP-20 balance to L1 when the zone is dead.

This proposal **is not**:

- A forced-inclusion mechanism. It cannot exit if a sequencer is *censoring* while still
  producing blocks.
- No "global" exit. Each user exits their own funds.
- Only TIP-20 funds. Funds held in other contracts are TBD. See [Open Questions](#7-open-questions).


## 1. Exit Notes & Nullifiers

To let users exit without the sequencer, Tempo needs an L1-verifiable record of *who owns what on the
zone* that does not depend on the sequencer being online. 

An **exit note** is a commitment to the  fact that: *"account `A` holds `balance` of token `T`."*
There is exactly **one live exit note per `(account, token)` pair**,
and it represents that account's  balance of that token. A note
is encrpyted, so the record can be published without leaking who owns how much.

When an account's balance changes, a new exit note is created and the previous one is invalidated. Invalidation is done by publishing the old note's **nullifier**. Publishing a
note's nullifier marks that note as spent  without revealing which account it belonged
to. So at any moment a note is **live and withdrawable** if, and only if, its nullifier has *not* been
published.

So, to forcibly exit the zone, a user proves two things 1. that the exit note exists in the record, and 2. that its nullifier has not
been published (i.e. the exit note is unspent). The portal then releases that balance on L1. 

Concretely, the sequencer maintains two structures alongside normal zone state and commits to their
roots on L1:

| Structure | Type | Contents |
|---|---|---|
| `exitNoteTree` | Append-only **incremental Merkle tree** | One leaf per note commitment ever created. The *latest* leaf for an `(account, token)` pair is that account's live balance note; earlier leaves for the same pair are superseded. |
| `exitNullifierTree` | **Sparse Merkle tree**, keyed by `H(nullifier)` | `1` = the note has been spent/superseded inside the zone; empty = still live and withdrawable. |

A note commitment binds the balance to the owner without revealing either:

```
noteId = H("noteId", zoneId, token, owner, version, epochId)
noteCommitment = Commit("note", noteId, balance, blinding)
nullifier      = H("nullifier", noteId, blinding) 
```

- `version` increments each time the account's note for that token is superseded, so every successive
  note for the same account has a unique `noteId` and therefore a unique nullifier, preserving privacy.
- `epochId` If we decide to do exit note tree epochs (See "State Growth and Expiry" below)
- `blinding` is a per-note random nonce so the commitment is hiding (two notes with the same balance
  still look different on-chain).

### 1.1 How a transfer updates the trees

A transfer rewrites the notes of every account whose balance
changes in the tx. Lets say Alice sends Bob 10 tokens. Exactly two accounts change, so the batch
adds **two nullifiers and two new note commitments**:

```
State before:   Alice note (balance 100, version k)
                Bob   note (balance  50, version m)

Transfer 10:    nullify Alice note  ->  add nullifier N_Alice_k
                nullify Bob   note  ->  add nullifier N_Bob_m
                create Alice note   ->  commitment C_Alice (balance 90,  version k+1)
                create Bob   note   ->  commitment C_Bob   (balance 60,  version m+1)

State after:    exitNoteTree     += [C_Alice, C_Bob]      (append-only)
                exitNullifierTree set H(N_Alice_k)=1, H(N_Bob_m)=1
```

**Note:** 

- **Deposit (mint)** and **withdrawal (burn)** change a single account: `1 nullifier (if a prior
  note exists) + 1 new note`.
- **Fees.** A transfer also credits the block beneficiary (sequencer). Strictly, that is a third
  changed account and a third note pair. To avoid one note-pair per transaction for the fee account,
  the sequencer SHOULD coalesce the beneficiary's balance into a single superseding note **once per
  block** rather than once per transfer. (See [Open Questions](#open-questions).)

The invariant that matters for solvency: **the sum of all *live* (un-nullified) exit notes for a
token equals that token's zone-side supply, which equals the portal's locked balance on Tempo.**

---

## 2. Data Availability

For users to exit without the sequencer, the per-(account, token) note data must be **available**
even when the sequencer is offline. With each `submitBatch` (per batch, *not* per block) the
sequencer publishes a DA blob.

### 2.1 What is published

```
BatchDAHeader:
  zoneId
  batchIndex
  prevBlockHash
  stateRootAfter           // the normal zone state root (already in the batch today)
  exitNoteRootAfter        // root of the exitNoteTree (incremental Merkle tree)
  exitNullifierRootAfter   // root of the exitNullifierTree (sparse Merkle trie)
  exitDataRoot             // binds the deltas + encrypted packets below
  withdrawalQueueHash      // for in-flight withdrawal edge cases
  tempoBlockNumber
  epochId                  // state-expiry epoch (see Section 5)
```

The header fields `exitNoteRootAfter`, `exitNullifierRootAfter`, `exitDataRoot`, and `epochId` are
**new** relative to the current `submitBatch`  parameters and would be stored by the
portal so they can be frozen on emergency activation.

Committed under `exitDataRoot`, the sequencer also writes the **exit deltas** (the blob body). These
contain ciphertext only, so they do not leak recipients/amounts to public observers  (though the
number of notes/nullifiers per batch may enable timing/volume side channels):

```
ExitDelta:
  spentNullifiers[]            // all nullifiers spent in this batch
  newNoteCommitments[]         // all new note commitments in this batch
  treeFrontierUpdates          // (optional) incremental-Merkle frontier so users can recompute paths
  encryptedExitPackets[]       // one per changed (account, token), decryptable by the owner
```

Each encrypted packet lets the owning user reconstruct their note off the public blob. The packet
itself is a public envelope; the sensitive fields live inside `ciphertext`, encrypted to the user's
key (see [Section 3](#3-encryption-keys--which-key-encrypts-a-users-exit-data)):

```
EncryptedExitPacket:
  noteCommitment    // the new note's commitment (also appears in newNoteCommitments[])
  ephemeralPubkey   // ECDH ephemeral key; combined with the user's key to derive the AES key
  ciphertext        // AES-GCM encryption of the PlainTextExitPacket below
```

When the owner decrypts `ciphertext`, they recover everything needed to rebuild the note and, later,
to prove it during an exit:

```
PlainTextExitPacket:
  zoneId
  owner
  token
  amount            // the account's balance recorded by this note
  version           // this note's version counter; needed to recompute noteId in the exit proof
  noteId            // identity of this note: H("noteId", zoneId, token, owner, version, epochId)
  blinding          // random nonce that goes into noteCommitment / nullifier
  noteLeafIndex     // position of noteCommitment in the exitNoteTree (for the inclusion proof)
  epochId           // which epoch's exitNoteTree this leaf lives in (see Section 5)
  relatedTxHash / depositNumber / withdrawalId   // provenance, for the user to reconcile
```

In other words, the public blob carries, for every changed `(account, token)`, both the new note's
commitment and an encrypted bundle that only the owner can open to learn the note's `amount`,
`version`, `blinding`, `noteId`, and its `noteLeafIndex` — exactly the witness an exit needs
(see [Section 6](#6-process-to-exit-end-to-end)). `version` is required so the user can recompute `noteId` (and from it the nullifier)
inside the exit proof; without it the `owner → noteId` opening in [Section 6.3](#63-submitting-an-exit) is not provable.

The DA per batch is roughly the size of the balance-changes in the batch — same order of magnitude as
the block data itself. Size estimates are in [Section 5.3](#53-size-and-growth-estimates).



### 2.2 Where the blob lives

The `exitDataRoot` (a 32-byte commitment) is cheap and always lives on Tempo L1 in the batch header.
The question is where the **blob body** is stored and who guarantees it is retrievable during an
emergency. Options, roughly from most-trust-minimized to most-operationally-simple:

| Option | Mechanism | Pros | Cons |
|---|---|---|---|
| **(A) DAC** | A Data Availability Committee — e.g. a subset of Tempo L1 validators holding signing keys — stores each blob and signs an availability attestation that the portal records. If the sequencer dies, the committee serves the data for exit. | Reuses L1 validator trust; attestations are on-chain and slashable; no single point of failure with `t`-of-`n`. | New committee to bootstrap, key management, liveness/incentives for the committee, honest-majority assumption. |
| **(B) External/Object storage** | Blobs pushed to an **S3 / Cloudflare R2** bucket (ideally write-once / object-lock, multi-region) or to some external storage | Trivial to operate, cheap, high durability. | Centralized and operator-controlled; the sequencer operator could withhold or delete; needs an independent mirror to be credible. |
| **(C) External DA layer** | Post blobs to **Celestia / EigenDA / Avail / NEAR DA**, recording the DA-layer commitment on Tempo. | Purpose-built DA with sampling + economic guarantees; decoupled from the sequencer. | Extra dependency and cost; cross-chain availability proof plumbing. |
 

---

## 3. Encryption Keys — Which Key Encrypts a User's Exit Data

Each `EncryptedExitPacket` must be encryptable **to the user** so only they can recover their note
from the public blob. The zone already has an ECIES-over-secp256k1 scheme and the
AES-GCM primitives for encrypted deposits; exit
packets reuse the same machinery, but the recipient is the *user's* key rather than the sequencer's.
The question is **which user key**, and how the sequencer learns it. Options:

### 3.1 Explicit registration (opt-in backup key)

The user registers a dedicated **backup / withdrawal public key** with the sequencer, via either an
RPC call or an on-chain zone transaction. The sequencer encrypts all of that user's exit
packets to this key.

- **Pros:** explicit, lets users separate their *exit/viewing* key from their *spending* key, and
  lets them point exits at a cold key.
- **Cons:** requires a user action; users who never register get no exit coverage, and privy/wallets/hardware wallets/etc... will need to support this.

### 3.2 Learn-on-first-transaction (implicit key)

The sequencer does **not** publish exit notes for an account until that account's **first on-chain
transaction**, from which the sequencer recovers the account's secp256k1 public key (the address is
already a hash of that key). It then encrypts exit packets to that key.

- **Pros:** zero extra user action; the key is exactly the one the user already controls.
- **Cons:** an account that has only ever *received* funds (deposits / incoming transfers) but never
  sent a transaction has no recovered public key, so **no exit packets are published for it** until
  it transacts. Couples viewing capability to the spending key.

### 3.3 Fallback to a third party (escrow until supplied)

If the sequencer does not know a user's key, it encrypts that user's exit packets to an
**independent third party's** public key (a designated escrow/custodian, ideally the same entity or
committee as the DAC). When the user later registers a key (3.1) or transacts (3.2), the sequencer
switches to the user's own key going forward.

- **Pros:** no balance is ever left without *some* recoverable exit path; the gap from 3.2 is closed.
- **Cons:** introduces a trusted third party who can read those users' balances; needs a defined
  custodian and an exit-assist flow during a freeze.
 

---

## 4. Dependency on the Prover

Can the escape hatch ship without a prover/verifier?

There are **two separate things a proof could secure**:

1. **The user's exit claim** (user → portal): "this note exists in the frozen tree, it is unspent,
   and I own it."
2. **The correctness of the frozen roots** (sequencer → everyone): "`exitNoteRoot` /
   `exitNullifierRoot` actually reflect every balance change, with no omitted or forged notes."


The exit claim (1) does **not** require the full batch prover/verifier while (2) will need a prover/verifier.


Without a prover/verifier that can verify (2), `submitBatch` simply *asserts*
`exitNoteRootAfter` and `exitNullifierRootAfter`; nothing forces them to be honest. A 
buggy sequencer could:

- **Omit a note** for a user (their balance becomes unexitable), or
- **Forge/over-issue notes** so total live notes exceed locked funds (insolvency at exit), or
- **Pre-spend** a nullifier so a live note appears spent.

Question: Can we simply trust that the sequencer is publishing the correct data as a v1, and when building out the full proover/verifier, we can add in checks for `exitNoteRoot` and `exitNullifierRoot` as a v2.


---

## 5. State Growth and Expiry

The two structures grow very differently.

- `exitNoteTree` is an **append-only incremental Merkle tree**: it grows by ~2 leaves per transfer
  forever, so both its size and its depth (`log₂(N)`) grow with cumulative transaction count. This is
  the structure we apply state expiry to.
- `exitNullifierTree` is a **fixed-depth sparse Merkle tree** keyed by `H(nullifier)`. Its depth is a
  constant (the keyspace, e.g. 256), so a non-membership proof is constant-size no matter how many
  entries it holds. Only its *populated leaf count* grows. **This structure never expires.**

### 5.1 Expiry applies to the exit-note tree only

Partition time into **epochs** (one year per epoch?). At each epoch boundary the sequencer **starts a
fresh `exitNoteTree`**. The previous epoch's note tree is frozen (its root kept on L1) and becomes a
read-only archive. The **nullifier set is a single, global, never-expiring sparse Merkle tree** shared
across all epochs.

A note **migrates forward on every spend**: when an account
transacts, its old note is nullified and the new note is appended to the *current* epoch's tree. So
active accounts naturally pull their live note into the latest epoch; an old epoch's note tree only
retains the notes of accounts that have been **dormant** since that epoch.

**Why expire the note tree but not the nullifier set.**

- *Note tree:* expiring it bounds the per-epoch tree size and inclusion-proof depth, and lets old
  epochs (once their still-live notes have migrated forward) move off the hot path.
- *Nullifier set:* it is **fixed-depth**, so resetting it would not shrink proofs. It would only force
  a user to supply *one non-membership proof per epoch since their note's origin* (to show the note
  was never spent in any later epoch), and it would risk nullifier collisions if `version` counters
  reset per epoch. Keeping one global set means **every exit needs exactly one non-membership
  proof** — no lookback fan-out, no cross-epoch collision concern.

### 5.2 Exit against an old epoch

Suppose it is **year 5**, the sequencer is down, and a dormant user
wants to exit a note that still lives in the **year-3** note tree. They:

1. Prove the note is **included** in the **frozen year-3 `exitNoteRoot`** (Merkle inclusion against
   that epoch's archived root), and
2. Prove the note's nullifier is **absent** from the **single global nullifier set** (one
   non-membership proof, fixed depth).


> **Correctness note:** because the nullifier set is global and never reset, `noteId` must be globally
> unique across epochs. 


### 5.3 Size and growth estimates

Per-item constants (order-of-magnitude):

| Item | Approx. size |
|---|---|
| Note commitment leaf | 32 B |
| Nullifier key (SMT leaf) | 32 B |
| Persisted note-tree cost per leaf (leaf + amortized internal nodes) | ~64–128 B |
| Persisted nullifier-tree cost per entry | ~64–100 B |
| `EncryptedExitPacket` (commitment 32 + ephemeral pubkey 33 + ciphertext + nonce 12 + tag 16) | ~260–290 B |
| **DA per transfer** (2 nullifiers + 2 commitments + 2 packets + amortized frontier) | **~0.7–1.1 KB** |

**Baseline assumption: 1 TPS** (1 yr ≈  ⇒ ~31.5M transfers/yr). A simple
transfer = 2 nullifiers + 2 notes, so the zone produces ~63M new note commitments and ~63M new
nullifiers per year.

| Metric (at 1 TPS) | Per year |
|---|---|
| Transfers | ~31.5 M |
| New note commitments | ~63 M |
| New nullifiers | ~63 M |
| `exitNoteTree` growth (**per epoch — reset yearly**) | ~4–8 GB (63M × ~64–128 B) |
| `exitNullifierTree` growth (**global — never reset**) | ~4–6 GB/yr, cumulative (63M × ~64–100 B) |
| **DA blob volume** | **~20–40 GB** (31.5M × ~0.7–1.1 KB) |

So at 1 TPS, each yearly epoch's note tree holds **~4–8 GB** (and is archived once its dormant notes
migrate forward), while the DA blobs run **~20–40 GB/yr**.  

**The global nullifier set.** Because it never expires, its *populated leaf count* grows
monotonically — ~63M entries/yr ⇒ ~4–6 GB/yr, so on the order of **~40–60 GB after a decade** at
1 TPS. The key point is that this growth is **storage only**: the tree is **fixed-depth** (e.g. 256
levels, the bit-length of `H(nullifier)`), so a non-membership proof is a constant ~256 sibling
hashes (~8 KB) *regardless* of how many entries exist.  

**Summary:** (1) DA volume is ~20–40 GB/yr. (2) Expiring the
note tree caps each epoch's note tree state that is held in memory in the sequencer at ~4–8 GB and lets old epochs be archived. (3) The global
nullifier set grows ~4–6 GB/yr forever but stays cheap to prove against because it is fixed-depth.

(Scaling is linear: a 10 TPS zone is ~10× these numbers.)

---

## 6. Process to Exit (End to End)

### 6.1 Freezing the zone

If the sequencer has not published a batch for a configurable window (proposal: **~1 month**) — or,
if a DAC is used, has failed to make DA available — **anyone** can place the zone's `ZonePortal`
into **emergency exit mode**. In this mode:

- Block production is frozen (it is already frozen if the sequencer is offline).
- The portal **snapshots ("freezes") the last-published roots**: the per-epoch `frozenExitNoteRoot`
  for each archived epoch, the single global `frozenExitNullifierRoot`, and the active `epochId`.
- Only emergency exit transactions are accepted.


### 6.2 Finding your note (recovery)

To exit, a user needs only their **single live note** for each token — the latest one, since there
is exactly one live note per `(account, token)` and every earlier note is already nullified. 


Start at the **tip** of the blob  and walk backward, attempting ECDH +
  AES-GCM decryption on each packet. Stop at the **first packet that decrypts** for a given token. If a wallet both sent and received the same token in
  one batch, two of its packets may appear together; disambiguate by taking the highest `version`. If the latest note is already nullified, that means the account's balance has been spent completely.
 

### 6.3 Submitting an exit

A user proves three things against the frozen roots: **their note exists**, **it was not already
spent before the zone froze**, and **the exit is authorized by the note's owner and pays out to
the owner** (so no one but the owner can withdraw a note, and only to the owner's own address). 

```
Private witness:                         Public inputs:
  note plaintext + blinding                zoneId, token, amount, recipient
  note Merkle inclusion path               frozenExitNoteRoot   (the note's origin epoch)
  nullifier non-membership path            frozenExitNullifierRoot (single, global)
                                           nullifier, originEpochId

The proof checks:
  1. noteId         = H("noteId", zoneId, token, owner, version, originEpochId)
  2. noteCommitment = Commit("note", noteId, amount, blinding)
  3. noteCommitment is included in frozenExitNoteRoot for originEpochId
  4. nullifier      = H("nullifier", noteId, blinding)
  5. H(nullifier) maps to EMPTY in the single global frozenExitNullifierRoot
  6. recipient is bound into the public inputs (it cannot be altered after submission)
  7. ownership — only the owner can withdraw, and only to their own address. recipient == owner. The owner field is public here, so the portal simply requires the destination to equal the note's committed owner.  
         

The portal checks:
  1. emergencyMode == true
  2. submitted roots match the frozen portal roots (note root for originEpochId; the global
     nullifier root)
  3. nullifier has not already been used in a prior exit (replay guard)
  4. (solvency) amount <= remaining locked balance for token
  5. recipient == owner — enforced on-chain 
```


### 6.4 Proving a nullifier is unspent

Non-membership = a single inclusion proof of the **default empty value** at the nullifier's key under
the one global frozen nullifier root (a spent note would have value `1` there). There is no
per-epoch loop — the global set captures every spend regardless of when it happened:

```
MerkleVerify(
  root  = frozenExitNullifierRoot,        // single, global, fixed-depth
  key   = H(nullifier),
  value = EMPTY,
  path  = nullifierNonMembershipPath
)
```

On success the portal releases `amount` of `token` from its locked balance to `recipient` and marks
the nullifier used.

---

## 7. Open Questions

- **In-flight at freeze.** What happens to deposits/withdrawals in-flight when the sequencer dies
  mid-batch — e.g. a withdrawal burned on the zone but never settled, or a deposit locked on L1 but
  never minted? The frozen roots reflect the last *published* batch; anything after it is covered by
  the deposit-queue refund / `bouncebackRecipient` paths on L1, but the seam needs to be specified.
  `withdrawalQueueHash` in the DA header is the hook for this.
- **Sequencer revival.** Can a sequencer come back and *disable* emergency mode? Proposal: once
  frozen, the zone stays frozen (exits are irreversible); a revived sequencer must deploy a new zone
  or follow a governed un-freeze with a long delay. Needs decision.
- **Non-TIP-20 value.** Funds owned by application contracts on the zone are out of scope. Do we need
  a per-contract exit hook, or is "TIP-20 balances only" acceptable for a first version?
- **Side channels.** Even encrypted, per-batch note/nullifier *counts* leak transaction volume and
  timing. Is padding / fixed-size batching worth the cost?
- **Dormant-account consolidation.** With the global nullifier set we never reset, exits stay
  single-proof, but old *note-tree* epochs can only be fully dropped once their dormant notes are
  re-noted forward (Section 5.1). Do we ever force this, and on what schedule, given we can't
  confiscate?
- **Freeze trigger.** Keyed only on "no batch in N days," or also on a DAC "DA withheld" attestation?
- **Is the account-note abstraction clear enough** for wallets to implement, or still too novel?
