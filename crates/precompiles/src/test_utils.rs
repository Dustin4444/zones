//! Shared test utilities for precompile tests.

use alloc::{rc::Rc, sync::Arc};
use core::cell::RefCell;
use std::{collections::HashMap, sync::Mutex};

use alloy_evm::{
    EvmInternals,
    precompiles::{DynPrecompile, Precompile as AlloyEvmPrecompile, PrecompileInput},
};
use alloy_primitives::{Address, B256, U256};
use k256::{
    AffinePoint, ProjectivePoint, Scalar,
    elliptic_curve::{ops::Reduce, sec1::ToEncodedPoint},
};
use revm::{
    Context,
    context::{BlockEnv, CfgEnv, TxEnv},
    database::{CacheDB, EmptyDB},
    precompile::{PrecompileError, PrecompileResult},
};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_precompiles::{
    storage::{
        Handler, PrecompileStorageProvider, StorageCtx, actions::StorageActions,
        evm::EvmPrecompileStorageProvider, hashmap::HashMapStorageProvider,
    },
    storage_credits::NonCreditableSlots,
    tip20::tip20_slots,
    tip403_registry::{PolicyData, TIP403Registry},
};

use crate::{
    L1BackedPrecompileEnv, L1StorageReader,
    chaum_pedersen::{challenge_hash, recover_point},
    ecies::DecryptedDeposit,
};

pub(crate) use crate::ecies::{build_plaintext, compressed_x_and_parity, encrypt_plaintext};

/// EVM context used by precompile tests.
pub(crate) type TestCtx = Context<BlockEnv, TxEnv, CfgEnv<TempoHardfork>, CacheDB<EmptyDB>>;

/// Create an empty EVM context for a precompile test.
pub(crate) fn test_context() -> TestCtx {
    Context::new(CacheDB::new(EmptyDB::new()), TempoHardfork::default())
}

/// Create a normal EVM storage provider over a test context.
pub(crate) fn test_storage_provider(
    ctx: &mut TestCtx,
    gas_limit: u64,
    is_static: bool,
) -> EvmPrecompileStorageProvider<'_> {
    let spec = ctx.cfg.spec;
    let amsterdam_eip8037_enabled = ctx.cfg.enable_amsterdam_eip8037;
    let gas_params = ctx.cfg.gas_params.clone();

    EvmPrecompileStorageProvider::new(
        EvmInternals::from_context(ctx),
        gas_limit,
        0,
        spec,
        amsterdam_eip8037_enabled,
        is_static,
        gas_params,
    )
}

/// Create the shared environment for an L1-backed precompile test.
pub(crate) fn test_l1_env<P: L1StorageReader>(
    ctx: &TestCtx,
    l1_reader: P,
) -> L1BackedPrecompileEnv<P> {
    L1BackedPrecompileEnv::new(
        &ctx.cfg,
        l1_reader,
        StorageActions::disabled(),
        Rc::new(RefCell::new(NonCreditableSlots::empty())),
    )
}

/// In-memory L1 storage reader shared by precompile tests.
#[derive(Clone)]
pub(crate) struct MockL1Reader {
    slots: Arc<Mutex<HashMap<(Address, B256), B256>>>,
    storage: Arc<Mutex<HashMapStorageProvider>>,
    fallback: B256,
    fail: bool,
    policy_id: u64,
}

impl Default for MockL1Reader {
    fn default() -> Self {
        Self {
            slots: Default::default(),
            storage: Arc::new(Mutex::new(HashMapStorageProvider::new(1))),
            fallback: B256::ZERO,
            fail: false,
            policy_id: 0,
        }
    }
}

impl MockL1Reader {
    pub(crate) fn allow_all() -> Self {
        Self::with_policy_id(1)
    }

    pub(crate) fn failing() -> Self {
        Self {
            fail: true,
            ..Self::allow_all()
        }
    }

    pub(crate) fn with_policy_id(policy_id: u64) -> Self {
        Self {
            policy_id,
            ..Default::default()
        }
    }

    pub(crate) fn returning(value: B256) -> Self {
        Self {
            fallback: value,
            ..Default::default()
        }
    }

    pub(crate) fn set_u256(&self, address: Address, slot: U256, value: U256) {
        self.slots.lock().unwrap().insert(
            (address, B256::from(slot.to_be_bytes())),
            B256::from(value.to_be_bytes()),
        );
    }

    pub(crate) fn seed_transfer_policy_id(&self, token: Address) {
        let packed = U256::from(self.policy_id) << (tip20_slots::TRANSFER_POLICY_ID_OFFSET * 8);
        self.set_u256(token, tip20_slots::TRANSFER_POLICY_ID, packed);
    }

    pub(crate) fn seed_blacklist_policy(
        &self,
        policy_id: u64,
        accounts: &[Address],
    ) -> tempo_precompiles::Result<()> {
        let mut storage = self.storage.lock().unwrap();
        StorageCtx::enter(&mut *storage, || {
            let mut registry = TIP403Registry::new();
            registry.policy_id_counter.write(policy_id + 1)?;
            registry.policy_records[policy_id].base.write(PolicyData {
                policy_type: tempo_contracts::precompiles::ITIP403Registry::PolicyType::BLACKLIST
                    as u8,
                admin: Address::ZERO,
            })?;
            for account in accounts {
                registry.policy_set[policy_id][*account].write(true)?;
            }
            Ok(())
        })
    }
}

impl crate::L1StorageReader for MockL1Reader {
    fn read_l1_storage(
        &self,
        account: Address,
        slot: B256,
        _block_number: u64,
    ) -> Result<B256, PrecompileError> {
        if self.fail {
            return Err(PrecompileError::Fatal("RPC unavailable".into()));
        }
        if let Some(value) = self.slots.lock().unwrap().get(&(account, slot)).copied() {
            return Ok(value);
        }

        let key = U256::from_be_bytes(slot.0);
        let value = self
            .storage
            .lock()
            .unwrap()
            .sload(account, key)
            .map_err(|err| PrecompileError::Fatal(err.to_string()))?;
        if value.is_zero() {
            Ok(self.fallback)
        } else {
            Ok(B256::from(value.to_be_bytes()))
        }
    }
}

/// Call a dynamic precompile with test defaults for value and reservoir.
pub(crate) fn call_precompile_with_gas(
    ctx: &mut TestCtx,
    precompile: &DynPrecompile,
    caller: Address,
    data: &[u8],
    gas: u64,
    is_static: bool,
    target: Address,
    code: Address,
) -> PrecompileResult {
    AlloyEvmPrecompile::call(
        precompile,
        PrecompileInput {
            data,
            gas,
            reservoir: 0,
            caller,
            value: U256::ZERO,
            target_address: target,
            is_static,
            bytecode_address: code,
            internals: EvmInternals::from_context(ctx),
        },
    )
}

#[rustfmt::skip]
/// Call a dynamic precompile with unlimited gas and test defaults.
pub(crate) fn call_precompile(
    ctx: &mut TestCtx,
    precompile: &DynPrecompile,
    caller: Address,
    data: &[u8],
    is_static: bool,
    target: Address,
    code: Address,
) -> PrecompileResult {
    call_precompile_with_gas(ctx, precompile, caller, data, u64::MAX, is_static, target, code)
}

/// Assert that the Chaum-Pedersen proof inside a [`DecryptedDeposit`] is valid.
pub(crate) fn assert_cp_proof_valid(
    dec: &DecryptedDeposit,
    ephemeral_pub: &AffinePoint,
    sequencer_pub: &AffinePoint,
) {
    let s = <Scalar as Reduce<k256::U256>>::reduce_bytes(&dec.proof.cp_proof_s.0.into());
    let c = <Scalar as Reduce<k256::U256>>::reduce_bytes(&dec.proof.cp_proof_c.0.into());
    let shared_pt =
        recover_point(&dec.proof.shared_secret.0, dec.proof.shared_secret_y_parity).unwrap();

    let r1 = ProjectivePoint::GENERATOR * s - ProjectivePoint::from(*sequencer_pub) * c;
    let r2 = ProjectivePoint::from(*ephemeral_pub) * s - ProjectivePoint::from(shared_pt) * c;

    let c_prime = challenge_hash(
        ephemeral_pub,
        sequencer_pub,
        &shared_pt,
        &r1.to_affine(),
        &r2.to_affine(),
    );
    assert_eq!(c, c_prime, "Chaum-Pedersen proof must verify");
}

/// Pre-computed encrypted deposit for testing.
/// All fields are deterministic (derived from fixed seed keys).
pub(crate) struct EncryptedDepositFixture {
    pub seq_key: k256::SecretKey,
    pub seq_pub: AffinePoint,
    pub eph_pub: AffinePoint,
    pub eph_pub_x: B256,
    pub eph_pub_y_parity: u8,
    pub portal: Address,
    pub key_index: U256,
    pub to: Address,
    pub memo: B256,
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; 12],
    pub tag: [u8; 16],
}

impl EncryptedDepositFixture {
    /// Create a fixture with deterministic keys for reproducible tests.
    pub(crate) fn new() -> Self {
        use sha2::{Digest, Sha256};

        // Deterministic sequencer key
        let seq_bytes: [u8; 32] = Sha256::digest(b"test-sequencer-key").into();
        let seq_key = k256::SecretKey::from_slice(&seq_bytes).expect("valid key");
        let seq_scalar: Scalar = *seq_key.to_nonzero_scalar();
        let seq_pub = AffinePoint::from(ProjectivePoint::GENERATOR * seq_scalar);

        // Deterministic ephemeral key
        let eph_bytes: [u8; 32] = Sha256::digest(b"test-ephemeral-key").into();
        let eph_key = k256::SecretKey::from_slice(&eph_bytes).expect("valid key");
        let eph_scalar: Scalar = *eph_key.to_nonzero_scalar();
        let eph_pub = AffinePoint::from(ProjectivePoint::GENERATOR * eph_scalar);
        let (eph_pub_x, eph_pub_y_parity) = compressed_x_and_parity(&eph_pub);

        // ECDH (depositor side)
        let shared_proj = ProjectivePoint::from(seq_pub) * eph_scalar;
        let shared_affine = AffinePoint::from(shared_proj);
        let ss_enc = shared_affine.to_encoded_point(true);
        let shared_secret_x: [u8; 32] = ss_enc.x().unwrap().as_slice().try_into().unwrap();

        let portal = Address::repeat_byte(0xAA);
        let key_index = U256::from(42u64);

        // HKDF key derivation
        let info = crate::ecies::hkdf_info(&portal, &key_index, &eph_pub_x);
        let aes_key = crate::ecies::hkdf_sha256(&shared_secret_x, b"ecies-aes-key", &info);

        // Build and encrypt plaintext
        let to = Address::repeat_byte(0xBB);
        let memo = B256::repeat_byte(0xCC);
        let plaintext = build_plaintext(&to, &memo);
        let (ciphertext, nonce, tag) = encrypt_plaintext(&aes_key, &plaintext);

        Self {
            seq_key,
            seq_pub,
            eph_pub,
            eph_pub_x,
            eph_pub_y_parity,
            portal,
            key_index,
            to,
            memo,
            ciphertext,
            nonce,
            tag,
        }
    }

    /// Decrypt using the fixture's sequencer key.
    pub(crate) fn decrypt(&self) -> Option<DecryptedDeposit> {
        crate::ecies::decrypt_deposit(
            &self.seq_key,
            &self.eph_pub_x,
            self.eph_pub_y_parity,
            &self.ciphertext,
            &self.nonce,
            &self.tag,
            self.portal,
            self.key_index,
        )
    }
}
