# Zone Escape Hatch

This crate contains the data structures for the Tempo
Zone escape hatch. The escape hatch is the emergency path that lets users recover
TIP-20 balances from a zone if its sequencer stops publishing batches.

## Core Model

Each live balance is represented by an exit note:

- There is one live note per `(owner, token)` pair.
- A note commits to the owner, token, balance, version, and a blinding value.
- The note commitment is appended to an exit-note Merkle tree.
- When a balance changes, the old note is invalidated by publishing its
  nullifier, and a new note is appended for the updated balance.

This gives the system two roots to track:

- `exitNoteRoot`: an append-only Merkle tree of every note commitment created.
- `exitNullifierRoot`: a sparse tree of spent note nullifiers.

A note is withdrawable if its commitment is included in the note tree and its
nullifier is absent from the nullifier tree.

## Batch Commitments

For every submitted batch, the sequencer computes the balance changes caused by
the batch and updates the escape-hatch state alongside normal zone execution.
The batch commitment includes:

- the note-tree root after the batch,
- the nullifier-tree root after the batch,
- a commitment to the published exit data for the batch,
- the current note-tree epoch.


## Exit Data

Users also need the private
note data required to reconstruct their commitment and nullifier.

For each changed `(owner, token)` balance, the sequencer publishes an encrypted
exit packet in the batch's exit data. The packet contains the user's new note
metadata, including the amount, version, blinding value, and leaf index. It is
encrypted to a user-controlled recovery key when available, with the user's
spending key as the fallback once that public key is known.


## Emergency Exit Flow

If the sequencer fails to publish batches for `X` blocks,
anyone can put the zone portal into emergency exit mode. At that point the portal
freezes:

- the latest note root for the active epoch,
- archived note roots for older epochs,
- the latest global nullifier root.

Users then recover their latest encrypted packet, rebuild their note, and submit
an exit proof to the portal. The proof shows that:

- the note commitment is included in the frozen note root for its epoch,
- the corresponding nullifier is absent from the frozen nullifier root,
- the exit is authorized for the note owner and token amount.

After verifying the proof and checking that the nullifier has not already been
used for an emergency withdrawal, the portal releases the locked L1 funds to the
owner and marks the nullifier consumed.

## Epochs and Growth

The note tree grows with every balance change, so it is split into epochs, one per year. 
Active accounts naturally move into the current
epoch when they transact, because their old note is nullified and their new note
is appended to the current tree.

The nullifier tree is global and does not reset. Keeping one global nullifier set
means an exit needs only one non-membership proof, even if the note originated in
an older epoch.
