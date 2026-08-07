use super::*;
use std::{collections::VecDeque, num::NonZeroUsize};

/// Finalized L1 blocks waiting to be processed by the Zone engine.
///
/// Tempo finality is deterministic, so this queue is append-only. Conflicting,
/// skipped, or disconnected finalized blocks are errors rather than forks to
/// reconcile locally.
#[derive(Debug, Default)]
pub(crate) struct PendingDeposits {
    /// Pending L1 blocks with their portal events, not yet processed by the Zone.
    pending: VecDeque<Arc<L1BlockDeposits>>,
    /// Highest L1 block ever enqueued (number + hash). Survives `confirm` /
    /// `drain` so that reconnecting subscribers know where the queue left off,
    /// even if the engine has already consumed the blocks.
    last_enqueued: Option<NumHash>,
}

impl PendingDeposits {
    /// Enqueue a finalized L1 block.
    ///
    /// Returns `true` when the block was appended and `false` for an exact
    /// redelivery of the current tip. Any other non-contiguous observation is a
    /// finality or provider-integrity failure and leaves the queue unchanged.
    pub(crate) fn try_enqueue(
        &mut self,
        header: SealedHeader<TempoHeader>,
        events: L1PortalEvents,
    ) -> eyre::Result<bool> {
        let block_number = header.number();
        let block_hash = header.hash();

        if let Some(last) = self.last_enqueued {
            if block_number < last.number {
                eyre::bail!(
                    "out-of-order finalized L1 block {block_number}; latest enqueued block is {}",
                    last.number
                );
            }
            if block_number == last.number {
                eyre::ensure!(
                    block_hash == last.hash,
                    "conflicting finalized L1 block at height {block_number}: \
                     existing={}, received={block_hash}",
                    last.hash
                );
                return Ok(false);
            }

            let expected = last
                .number
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("finalized L1 block number overflow"))?;
            eyre::ensure!(
                block_number == expected,
                "non-contiguous finalized L1 block: expected {expected}, received {block_number}"
            );
            eyre::ensure!(
                header.parent_hash() == last.hash,
                "finalized L1 parent mismatch at height {block_number}: \
                 expected={}, received={}",
                last.hash,
                header.parent_hash()
            );
        }

        self.last_enqueued = Some(header.num_hash());
        self.pending
            .push_back(Arc::new(L1BlockDeposits { header, events }));
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn enqueue(&mut self, header: TempoHeader, events: L1PortalEvents) {
        self.try_enqueue(SealedHeader::seal_slow(header), events)
            .expect("test finalized blocks must be contiguous");
    }

    /// Peek a bounded contiguous range for one atomic Zone advance.
    ///
    /// NOTE: A leader-transition block starts a new range unless it is already the first block.
    ///
    /// Returns `None` if no L1 blocks are queued. Use [`confirm`](Self::confirm) after a
    /// successful build to advance the queue. Use
    /// [`peek_with_leadership`](Self::peek_with_leadership) to also enforce effective leadership
    /// boundaries.
    pub(crate) fn peek(&self, max_headers: NonZeroUsize) -> Option<Vec<Arc<L1BlockDeposits>>> {
        self.peek_with_leadership(max_headers, |_| None::<()>)
    }

    /// Peek a bounded contiguous range governed by one effective leadership record.
    ///
    /// NOTE: A change from the first block's effective leadership record starts a new range. This
    /// catches boundaries, such as forced recovery, that may not emit a leader-transition event.
    /// Otherwise, it has the same portal-transition and confirmation semantics as
    /// [`peek`](Self::peek).
    pub(crate) fn peek_with_leadership<R: PartialEq>(
        &self,
        max_headers: NonZeroUsize,
        mut leadership_for: impl FnMut(u64) -> Option<R>,
    ) -> Option<Vec<Arc<L1BlockDeposits>>> {
        let first_record = leadership_for(self.pending.front()?.header.number());
        let mut range = Vec::with_capacity(self.pending.len().min(max_headers.get()));
        for block in self.pending.iter().take(max_headers.get()) {
            // A portal transition block starts the next range. Keep it when it is the first
            // header, but never let it become the final header of the outgoing range.
            if !range.is_empty() && !block.events.leader_transitions.is_empty() {
                break;
            }
            if !range.is_empty()
                && leadership_for(block.header.number()).as_ref() != first_record.as_ref()
            {
                break;
            }
            range.push(block.clone());
        }
        Some(range)
    }

    /// Strictly confirm one producer-selected range and remove exactly that range.
    ///
    /// All selected headers are validated before the queue is mutated. Taking the selected range
    /// itself avoids passing independently derived anchors and lengths that could disagree.
    pub(crate) fn confirm(&mut self, selected: &[Arc<L1BlockDeposits>]) -> eyre::Result<()> {
        eyre::ensure!(
            !selected.is_empty(),
            "cannot confirm an empty finalized L1 range"
        );
        eyre::ensure!(
            selected.len() <= self.pending.len(),
            "finalized L1 range confirmation is too long: selected {} entries, only {} are pending",
            selected.len(),
            self.pending.len()
        );
        for (index, (queued, selected)) in self.pending.iter().zip(selected).enumerate() {
            eyre::ensure!(
                Arc::ptr_eq(queued, selected),
                "finalized L1 range confirmation mismatch at index {index}: selected {:?}, queued {:?}",
                selected.header.num_hash(),
                queued.header.num_hash()
            );
        }
        drop(self.pending.drain(..selected.len()));
        Ok(())
    }

    /// Confirm every pending L1 block up to and including `expected`.
    ///
    /// Follower import calls this only after the corresponding zone block is canonical. It is
    /// intentionally idempotent for an already-consumed anchor, but prevalidates the target before
    /// removing any stale entries so a hash conflict or missing target leaves the queue unchanged.
    pub(crate) fn confirm_through(&mut self, expected: NumHash) -> eyre::Result<()> {
        let Some(front) = self.pending.front() else {
            return Ok(());
        };
        if front.header.number() > expected.number {
            return Ok(());
        }

        let target_index = usize::try_from(expected.number - front.header.number())
            .map_err(|_| eyre::eyre!("finalized L1 range exceeds addressable memory"))?;
        let target = self.pending.get(target_index).ok_or_else(|| {
            eyre::eyre!(
                "cannot confirm through absent finalized L1 block {}",
                expected.number
            )
        })?;
        eyre::ensure!(
            target.header.hash() == expected.hash,
            "deposit queue holds L1 block {} with hash {}, but the consumed block is {}",
            target.header.number(),
            target.header.hash(),
            expected.hash,
        );

        drop(self.pending.drain(..=target_index));
        Ok(())
    }

    /// Drain all pending L1 block deposits.
    #[cfg(test)]
    pub(crate) fn drain(&mut self) -> Vec<Arc<L1BlockDeposits>> {
        self.pending.drain(..).collect()
    }

    /// Returns the number of pending L1 blocks.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns the most recently enqueued L1 block (number + hash), if any.
    pub(crate) fn last_enqueued(&self) -> Option<NumHash> {
        self.last_enqueued
    }
}

/// Shared deposit queue with notification support.
///
/// Wraps the pending deposits with a `Notify` so the ZoneEngine can be
/// woken instantly when new L1 blocks arrive.
#[derive(Debug, Clone)]
pub struct DepositQueue {
    inner: Arc<Mutex<PendingDeposits>>,
    notify: Arc<tokio::sync::Notify>,
}

impl DepositQueue {
    /// Create a new empty deposit queue.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PendingDeposits::default())),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Enqueue a finalized L1 block and notify waiters when it was appended.
    pub fn enqueue(&self, header: TempoHeader, events: L1PortalEvents) {
        self.enqueue_sealed(SealedHeader::seal_slow(header), events);
    }

    /// Enqueue an already-sealed header and report invariant violations to the
    /// caller instead of panicking.
    ///
    /// Subscriber and peer-import inputs are external, so they use this path.
    /// Returns whether the block was newly appended.
    pub fn try_enqueue_sealed(
        &self,
        header: SealedHeader<TempoHeader>,
        events: L1PortalEvents,
    ) -> eyre::Result<bool> {
        let mut queue = self.inner.lock();
        if let Some(queued) = queue
            .pending
            .iter()
            .find(|queued| queued.header.number() == header.number())
        {
            eyre::ensure!(
                queued.header.hash() == header.hash(),
                "conflicting finalized L1 block at height {}: existing={}, received={}",
                header.number(),
                queued.header.hash(),
                header.hash()
            );
            return Ok(false);
        }
        let appended = queue.try_enqueue(header, events)?;
        drop(queue);
        if appended {
            self.notify.notify_one();
        }
        Ok(appended)
    }

    /// Like [`enqueue`](Self::enqueue) but accepts an already-sealed header,
    /// avoiding a redundant hash computation.
    pub fn enqueue_sealed(
        &self,
        header: SealedHeader<TempoHeader>,
        events: L1PortalEvents,
    ) -> bool {
        let appended = self
            .inner
            .lock()
            .try_enqueue(header, events)
            .unwrap_or_else(|err| panic!("finalized L1 queue invariant violated: {err}"));
        if appended {
            self.notify.notify_one();
        }
        appended
    }

    /// Peek at the next non-empty contiguous L1 block range, bounded by portal-recorded leader
    /// transitions, without removing it.
    ///
    /// Use [`Self::peek_with_leadership`] to also enforce effective leadership boundaries.
    pub fn peek(&self, max_headers: NonZeroUsize) -> Option<Vec<Arc<L1BlockDeposits>>> {
        self.inner.lock().peek(max_headers)
    }

    /// Like [`Self::peek`], but also bounds the range at effective leadership changes.
    pub fn peek_with_leadership<R: PartialEq>(
        &self,
        max_headers: NonZeroUsize,
        leadership_for: impl FnMut(u64) -> Option<R>,
    ) -> Option<Vec<Arc<L1BlockDeposits>>> {
        self.inner
            .lock()
            .peek_with_leadership(max_headers, leadership_for)
    }

    /// Confirm a L1 block range was successfully processed and remove it.
    pub fn confirm(&self, selected: &[Arc<L1BlockDeposits>]) -> eyre::Result<()> {
        self.inner.lock().confirm(selected)
    }

    /// Advance the queue past a canonical follower anchor.
    pub fn confirm_through(&self, expected: NumHash) -> eyre::Result<()> {
        self.inner.lock().confirm_through(expected)
    }

    /// Wait until an L1 block is available.
    pub async fn notified(&self) {
        self.notify.notified().await
    }

    /// Returns the most recently enqueued L1 block (number + hash), if any.
    ///
    /// This is a high-water mark that survives `confirm` / `drain`, so it
    /// reflects the last block ever enqueued — not just what's still pending.
    pub fn last_enqueued(&self) -> Option<NumHash> {
        self.inner.lock().last_enqueued()
    }

    #[cfg(test)]
    pub(crate) fn drain(&self) -> Vec<Arc<L1BlockDeposits>> {
        self.inner.lock().drain()
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.inner.lock().pending_len()
    }
}

impl Default for DepositQueue {
    fn default() -> Self {
        Self::new()
    }
}
