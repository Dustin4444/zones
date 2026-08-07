use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};

use super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    encoding::{DepositQueueMember, withdrawal_queue_hash},
    input::{
        AuthenticatedDepositOutcome, AuthenticatedWithdrawalOutcome, BatchFinalizationInput,
        ImportedTempoBlockInput, ImportedTempoOperation, RefundClaimInput, ZoneBlockContext,
        ZoneBlockInput, ZoneDepositPrefixInput, ZoneOperation,
    },
    ownership::{
        BatchOwner, DepositOwner, FallbackOwner, InboxRefundOwner, PendingWithdrawal,
        PortalRefundOwner, WithdrawalOwner,
    },
    state::{ModelState, PortalIdentity},
    transition::{ModelStateUpdate, ModelTransition},
};

const TOKEN: Address = Address::repeat_byte(0xe1);
const AMOUNT: u128 = 40;
const USER_AMOUNT: u128 = 20;
const GENESIS_HASH: B256 = B256::repeat_byte(0x10);
const GENESIS_TEMPO_HASH: B256 = B256::repeat_byte(0x20);

pub(crate) struct LifecycleRecoveryScenario {
    pub(crate) portal_identity: PortalIdentity,
    pub(crate) initial_state: ModelState,
    pub(crate) initial_zone_tip: BlockNumHash,
    pub(crate) initial_tempo_tip: BlockNumHash,
    pub(crate) steps: Vec<RecoveryStep>,
}

#[derive(Clone)]
pub(crate) struct RecoveryStep {
    imported: ImportedTempoBlockInput,
    zone: ZoneBlockInput,
    pub(crate) zone_tip: BlockNumHash,
    pub(crate) tempo_tip: BlockNumHash,
    pub(crate) expected_state: ModelState,
    pub(crate) checkpoint: Option<&'static str>,
}

impl RecoveryStep {
    pub(crate) fn state_update(&self, parent: &ModelState) -> ModelStateUpdate {
        ModelTransition::new(parent)
            .apply_imported_tempo_block(&self.imported)
            .unwrap()
            .apply_zone_block(&self.zone)
            .unwrap()
            .into_state_update()
    }
}

pub(crate) fn lifecycle_recovery_scenario() -> LifecycleRecoveryScenario {
    let mut builder = ScenarioBuilder::new();
    exercise_failed_deposit_phases(&mut builder);
    exercise_user_phases(&mut builder);
    builder.finish()
}

struct ScenarioBuilder {
    portal_identity: PortalIdentity,
    initial_state: ModelState,
    state: ModelState,
    initial_zone_tip: BlockNumHash,
    initial_tempo_tip: BlockNumHash,
    zone_tip: BlockNumHash,
    tempo_tip: BlockNumHash,
    steps: Vec<RecoveryStep>,
}

impl ScenarioBuilder {
    fn new() -> Self {
        let mut state = created_state(TOKEN);
        state.set_token_accounting_for_test(
            TOKEN,
            TokenAccounting {
                supply: U256::from(100),
                ..TokenAccounting::ZERO
            },
        );
        let portal_identity = identity(TOKEN);
        let initial_zone_tip = BlockNumHash::new(0, GENESIS_HASH);
        let initial_tempo_tip = BlockNumHash::new(0, GENESIS_TEMPO_HASH);
        Self {
            portal_identity,
            initial_state: state.clone(),
            state,
            initial_zone_tip,
            initial_tempo_tip,
            zone_tip: initial_zone_tip,
            tempo_tip: initial_tempo_tip,
            steps: Vec::new(),
        }
    }

    fn apply(
        &mut self,
        imported_operations: Vec<ImportedTempoOperation>,
        advance: ZoneDepositPrefixInput,
        zone_operations: Vec<ZoneOperation>,
        finalization: Option<BatchFinalizationInput>,
    ) {
        let number = self.zone_tip.number + 1;
        let step = RecoveryStep {
            imported: ImportedTempoBlockInput::new(number, U256::ZERO, imported_operations),
            zone: ZoneBlockInput::new(
                ZoneBlockContext::new(B256::repeat_byte(number as u8), number),
                advance,
                zone_operations,
                finalization,
            ),
            zone_tip: BlockNumHash::new(number, B256::repeat_byte(number as u8)),
            tempo_tip: BlockNumHash::new(number, B256::repeat_byte(0x80 + number as u8)),
            expected_state: self.state.clone(),
            checkpoint: None,
        };
        let update = step.state_update(&self.state);
        update.apply_to_current_parent(&mut self.state);
        self.zone_tip = step.zone_tip;
        self.tempo_tip = step.tempo_tip;
        self.steps.push(RecoveryStep {
            expected_state: self.state.clone(),
            ..step
        });
    }

    fn checkpoint(&mut self, label: &'static str) {
        let step = self.steps.last_mut().expect("checkpoint follows one step");
        assert!(step.checkpoint.replace(label).is_none());
    }

    fn finish(self) -> LifecycleRecoveryScenario {
        LifecycleRecoveryScenario {
            portal_identity: self.portal_identity,
            initial_state: self.initial_state,
            initial_zone_tip: self.initial_zone_tip,
            initial_tempo_tip: self.initial_tempo_tip,
            steps: self.steps,
        }
    }
}

fn exercise_failed_deposit_phases(builder: &mut ScenarioBuilder) {
    let deposit = ordinary(TOKEN, 0xe2, AMOUNT);
    let member = DepositQueueMember::Ordinary(deposit.clone());

    builder.apply(
        vec![ImportedTempoOperation::OrdinaryDepositAppended(
            deposit.clone(),
        )],
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        None,
    );
    assert!(matches!(
        builder.state.pending_deposit(deposit_id(1)),
        Some(DepositOwner::PendingOrdinary { preimage }) if preimage == &deposit
    ));
    builder.checkpoint("ordinary deposit pending");

    builder.apply(
        Vec::new(),
        ZoneDepositPrefixInput::new(
            Vec::new(),
            vec![member],
            vec![AuthenticatedDepositOutcome::OrdinaryFailed],
        ),
        Vec::new(),
        None,
    );
    assert!(matches!(
        builder.state.withdrawal(withdrawal_id(0)),
        Some(WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(
            _
        )))
    ));
    builder.checkpoint("failed-deposit withdrawal pending");

    builder.apply(
        Vec::new(),
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        Some(BatchFinalizationInput::new(1, 3, vec![Default::default()])),
    );
    assert!(matches!(
        builder.state.batch(batch_id(1)),
        Some(BatchOwner::Finalized(_))
    ));
    builder.checkpoint("failed-deposit batch finalized");

    let submission = exact_submission(&builder.state, batch_id(1));
    builder.apply(
        vec![ImportedTempoOperation::BatchSubmitted(Box::new(submission))],
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        None,
    );
    assert!(matches!(
        builder.state.batch(batch_id(1)),
        Some(BatchOwner::Submitted(_))
    ));
    builder.checkpoint("failed-deposit batch submitted");

    let failed = finalized_preimage(&builder.state, 0);
    builder.apply(
        vec![withdrawals_processed(
            vec![failed],
            B256::ZERO,
            vec![AuthenticatedWithdrawalOutcome::FailedDepositPending],
        )],
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        None,
    );
    let portal_refund = portal_refund_id(TOKEN, deposit.tempo_refund_recipient(), 1);
    assert_eq!(
        builder.state.portal_refund(portal_refund),
        Some(&PortalRefundOwner::Pending { amount: AMOUNT })
    );
    builder.checkpoint("Portal refund credit pending");

    builder.apply(
        vec![ImportedTempoOperation::PortalRefundClaimed(
            RefundClaimInput::new(deposit.tempo_refund_recipient(), TOKEN, AMOUNT),
        )],
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        None,
    );
}

fn exercise_user_phases(builder: &mut ScenarioBuilder) {
    builder.apply(
        Vec::new(),
        ZoneDepositPrefixInput::default(),
        vec![
            ZoneOperation::user_withdrawal_accepted(user_withdrawal(
                TOKEN,
                0xe3,
                USER_AMOUNT,
                0,
                Default::default(),
            )),
            ZoneOperation::user_withdrawal_accepted(user_withdrawal(
                TOKEN,
                0xe4,
                USER_AMOUNT,
                0,
                Default::default(),
            )),
        ],
        None,
    );
    assert!(matches!(
        builder.state.withdrawal(withdrawal_id(1)),
        Some(WithdrawalOwner::Pending(PendingWithdrawal::User(_)))
    ));
    assert!(matches!(
        builder.state.fallback_owner(fallback_id(1)),
        Some(FallbackOwner::Held { withdrawal, .. }) if *withdrawal == withdrawal_id(1)
    ));
    assert!(matches!(
        builder.state.fallback_owner(fallback_id(2)),
        Some(FallbackOwner::Held { withdrawal, .. }) if *withdrawal == withdrawal_id(2)
    ));
    builder.checkpoint("user withdrawal and fallback pending");

    builder.apply(
        Vec::new(),
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        Some(BatchFinalizationInput::new(
            2,
            8,
            vec![Default::default(), Default::default()],
        )),
    );
    assert!(matches!(
        builder.state.batch(batch_id(2)),
        Some(BatchOwner::Finalized(_))
    ));
    builder.checkpoint("user batch finalized");

    let submission = exact_submission(&builder.state, batch_id(2));
    builder.apply(
        vec![ImportedTempoOperation::BatchSubmitted(Box::new(submission))],
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        None,
    );
    assert!(matches!(
        builder.state.batch(batch_id(2)),
        Some(BatchOwner::Submitted(_))
    ));
    builder.checkpoint("user batch submitted");

    let first = finalized_preimage(&builder.state, 1);
    let second = finalized_preimage(&builder.state, 2);
    let remaining = withdrawal_queue_hash(std::slice::from_ref(&second));
    builder.apply(
        vec![withdrawals_processed(
            vec![first],
            remaining,
            vec![AuthenticatedWithdrawalOutcome::user_delivered(Vec::new())],
        )],
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        None,
    );
    assert!(matches!(
        builder.state.batch(batch_id(2)),
        Some(BatchOwner::Submitted(batch))
            if batch.next_processing_ordinal() == 1
                && batch.remaining_queue_hash() == remaining
    ));
    assert!(builder.state.withdrawal(withdrawal_id(1)).is_none());
    assert!(builder.state.fallback_owner(fallback_id(1)).is_none());
    assert!(builder.state.withdrawal(withdrawal_id(2)).is_some());
    builder.checkpoint("partially processed submitted suffix");

    builder.apply(
        vec![withdrawals_processed(
            vec![second],
            B256::ZERO,
            vec![AuthenticatedWithdrawalOutcome::UserBounced],
        )],
        ZoneDepositPrefixInput::default(),
        Vec::new(),
        None,
    );
    assert!(matches!(
        builder.state.pending_deposit(deposit_id(2)),
        Some(DepositOwner::PendingWithdrawalBounceBack { withdrawal, .. })
            if *withdrawal == withdrawal_id(2)
    ));
    assert!(matches!(
        builder.state.fallback_owner(fallback_id(2)),
        Some(FallbackOwner::BounceBackQueued { deposit, .. }) if *deposit == deposit_id(2)
    ));
    builder.checkpoint("withdrawal bounce-back queued");

    let recipient = Address::repeat_byte(0xe6);
    builder.apply(
        Vec::new(),
        ZoneDepositPrefixInput::new(
            Vec::new(),
            vec![DepositQueueMember::WithdrawalBounceBack(bounce(
                TOKEN,
                2,
                USER_AMOUNT,
            ))],
            vec![AuthenticatedDepositOutcome::WithdrawalBounceBackPending { recipient }],
        ),
        Vec::new(),
        None,
    );
    let inbox_refund = inbox_refund_id(TOKEN, recipient, 2);
    assert!(matches!(
        builder.state.inbox_refund(inbox_refund),
        Some(InboxRefundOwner::Pending { amount }) if amount.get() == USER_AMOUNT
    ));
    builder.checkpoint("Inbox refund credit pending");
}
