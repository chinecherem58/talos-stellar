//! TalosDividends — Soroban smart contract for dividend distribution.
//!
//! ## Pause domains
//!
//! | Domain symbol | Entry point(s) guarded        | When to use                                    |
//! |---------------|-------------------------------|------------------------------------------------|
//! | `"commit"`    | [`commit_epoch`]              | Halt new epoch commits during an incident      |
//! | `"claim"`     | [`claim_dividend`]            | Halt patron claims during an incident          |
//!
//! ### Recovery while paused
//!
//! [`recover_expired`] is intentionally **not** gated by any pause domain.
//! Recovery is a maintenance operation that expires stale epoch entries and
//! keeps storage healthy; blocking it during an incident would worsen the
//! situation.  An operator that needs to freeze recovery in an extreme edge
//! case should use the standard pause mechanism at the application layer
//! (e.g., revoke the admin key) rather than relying on this contract.
//!
//! ### Pause / unpause flow
//!
//! ```text
//! admin ──► pause(admin, "commit")        # halts commit_epoch
//! admin ──► pause(admin, "claim")         # halts claim_dividend
//!                ↓  incident resolved
//! admin ──► unpause(admin, "claim")       # restores claiming
//! admin ──► unpause(admin, "commit")      # restores committing
//! ```
//!
//! Only the contract admin can pause or unpause domains.  There is no
//! time-lock on pause/unpause to allow a rapid incident response.

#![no_std]

#[cfg(all(test, not(target_arch = "wasm32")))]
extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};
use pause_control::{check_not_paused, is_paused, pause_domain, unpause_domain, PauseError};

// ── Pause domains ─────────────────────────────────────────────────────────────

/// Domain that gates [`TalosDividends::commit_epoch`].
pub const DOMAIN_COMMIT: &str = "commit";
/// Domain that gates [`TalosDividends::claim_dividend`].
pub const DOMAIN_CLAIM: &str = "claim";

// ── Data types ────────────────────────────────────────────────────────────────

/// A dividend epoch: total revenue collected and the supply snapshot used for
/// pro-rata calculations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Epoch {
    /// Sequential epoch identifier (1-based).
    pub id: u32,
    /// Total revenue (in stroops / smallest token unit) for this epoch.
    pub total_revenue: i128,
    /// Total token supply snapshot at epoch close.
    pub total_supply_snapshot: i128,
    /// Ledger sequence at which the epoch was committed.
    pub committed_at: u32,
    /// Whether this epoch has been marked as expired by [`recover_expired`].
    pub expired: bool,
}

/// Contract error variants.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DividendError {
    /// Attempted to initialize an already-initialized contract.
    AlreadyInitialized,
    /// The caller is not the contract admin.
    Unauthorized,
    /// The epoch does not exist.
    EpochNotFound,
    /// The patron has already claimed dividends for this epoch.
    AlreadyClaimed,
    /// The epoch has been expired via [`recover_expired`].
    EpochExpired,
    /// The operation is blocked because the domain is paused.
    Paused,
    /// The revenue or supply values are invalid (e.g. ≤ 0).
    InvalidAmount,
}

/// Storage keys.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Admin,
    NextEpochId,
    Epoch(u32),
    Claimed(u32, Address),
}

// ── Events ────────────────────────────────────────────────────────────────────

fn emit_epoch_committed(env: &Env, epoch_id: u32, total_revenue: i128, total_supply: i128) {
    env.events().publish(
        (symbol_short!("ep_commit"), epoch_id),
        (total_revenue, total_supply),
    );
}

fn emit_dividend_claimed(env: &Env, epoch_id: u32, patron: Address, amount: i128) {
    env.events().publish(
        (symbol_short!("div_claim"), epoch_id),
        (patron, amount),
    );
}

fn emit_epoch_recovered(env: &Env, epoch_id: u32) {
    env.events()
        .publish((symbol_short!("ep_recov"), epoch_id), ());
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct TalosDividends;

#[contractimpl]
impl TalosDividends {
    // ── Lifecycle ─────────────────────────────────────────────────────────────

    /// Initialize the contract.  Can only be called once.
    pub fn initialize(env: Env, admin: Address) -> Result<(), DividendError> {
        admin.require_auth();
        if env.storage().persistent().has(&DataKey::Admin) {
            return Err(DividendError::AlreadyInitialized);
        }
        env.storage().persistent().set(&DataKey::Admin, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::NextEpochId, &1u32);
        Ok(())
    }

    // ── Epoch management ──────────────────────────────────────────────────────

    /// Commit a new dividend epoch.
    ///
    /// **Gated by the `"commit"` pause domain.**  If the domain is paused,
    /// returns [`DividendError::Paused`].
    pub fn commit_epoch(
        env: Env,
        admin: Address,
        total_revenue: i128,
        total_supply_snapshot: i128,
    ) -> Result<u32, DividendError> {
        Self::require_admin(&env, &admin)?;

        // ── Pause check ──────────────────────────────────────────────────────
        check_not_paused(&env, Symbol::new(&env, DOMAIN_COMMIT))
            .map_err(|_| DividendError::Paused)?;

        if total_revenue <= 0 {
            return Err(DividendError::InvalidAmount);
        }
        if total_supply_snapshot <= 0 {
            return Err(DividendError::InvalidAmount);
        }

        let epoch_id = Self::next_epoch_id(&env);
        let epoch = Epoch {
            id: epoch_id,
            total_revenue,
            total_supply_snapshot,
            committed_at: env.ledger().sequence(),
            expired: false,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Epoch(epoch_id), &epoch);
        env.storage()
            .persistent()
            .set(&DataKey::NextEpochId, &(epoch_id + 1));

        emit_epoch_committed(&env, epoch_id, total_revenue, total_supply_snapshot);
        Ok(epoch_id)
    }

    // ── Claiming ──────────────────────────────────────────────────────────────

    /// Claim a patron's pro-rata dividend for a given epoch.
    ///
    /// **Gated by the `"claim"` pause domain.**  If the domain is paused,
    /// returns [`DividendError::Paused`].
    ///
    /// The amount returned is `(patron_balance / total_supply) * total_revenue`.
    pub fn claim_dividend(
        env: Env,
        patron: Address,
        epoch_id: u32,
        patron_balance: i128,
    ) -> Result<i128, DividendError> {
        patron.require_auth();

        // ── Pause check ──────────────────────────────────────────────────────
        check_not_paused(&env, Symbol::new(&env, DOMAIN_CLAIM))
            .map_err(|_| DividendError::Paused)?;

        let epoch: Epoch = env
            .storage()
            .persistent()
            .get(&DataKey::Epoch(epoch_id))
            .ok_or(DividendError::EpochNotFound)?;

        if epoch.expired {
            return Err(DividendError::EpochExpired);
        }

        let claim_key = DataKey::Claimed(epoch_id, patron.clone());
        if env.storage().persistent().has(&claim_key) {
            return Err(DividendError::AlreadyClaimed);
        }

        if patron_balance <= 0 {
            return Err(DividendError::InvalidAmount);
        }

        // Pro-rata calculation (integer arithmetic; no floating point in WASM)
        let amount = (patron_balance * epoch.total_revenue) / epoch.total_supply_snapshot;

        env.storage().persistent().set(&claim_key, &true);

        emit_dividend_claimed(&env, epoch_id, patron, amount);
        Ok(amount)
    }

    // ── Recovery ──────────────────────────────────────────────────────────────

    /// Mark an epoch as expired, preventing further claims.
    ///
    /// **Not gated by any pause domain** — see the module-level doc for the
    /// rationale.  Recovery should always be available to operators so they
    /// can keep storage healthy even during an incident.
    pub fn recover_expired(env: Env, admin: Address, epoch_id: u32) -> Result<(), DividendError> {
        Self::require_admin(&env, &admin)?;

        let mut epoch: Epoch = env
            .storage()
            .persistent()
            .get(&DataKey::Epoch(epoch_id))
            .ok_or(DividendError::EpochNotFound)?;

        epoch.expired = true;
        env.storage()
            .persistent()
            .set(&DataKey::Epoch(epoch_id), &epoch);

        emit_epoch_recovered(&env, epoch_id);
        Ok(())
    }

    // ── Pause controls ────────────────────────────────────────────────────────

    /// Pause a domain.  Only the contract admin may call this.
    ///
    /// `domain` must be one of `"commit"` or `"claim"`.
    pub fn pause(env: Env, admin: Address, domain: Symbol) -> Result<(), DividendError> {
        Self::require_admin(&env, &admin)?;
        pause_domain(&env, domain);
        Ok(())
    }

    /// Unpause a domain.  Only the contract admin may call this.
    pub fn unpause(env: Env, admin: Address, domain: Symbol) -> Result<(), DividendError> {
        Self::require_admin(&env, &admin)?;
        unpause_domain(&env, domain);
        Ok(())
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Return the epoch struct, or `None` if it does not exist.
    pub fn get_epoch(env: Env, epoch_id: u32) -> Option<Epoch> {
        env.storage().persistent().get(&DataKey::Epoch(epoch_id))
    }

    /// Return the current admin, or `None` if not initialized.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::Admin)
    }

    /// Return `true` when the given *domain* is currently paused.
    pub fn is_domain_paused(env: Env, domain: Symbol) -> bool {
        is_paused(&env, domain)
    }

    /// Return the next epoch ID that would be assigned by [`commit_epoch`].
    pub fn next_epoch_id(env: &Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::NextEpochId)
            .unwrap_or(1)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Address) -> Result<(), DividendError> {
        caller.require_auth();
        let stored: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(DividendError::Unauthorized)?;
        if stored != *caller {
            return Err(DividendError::Unauthorized);
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _, MockAuth, MockAuthInvoke},
        IntoVal,
    };

    // ── Setup helpers ──────────────────────────────────────────────────────────

    fn setup() -> (Env, Address, Address, TalosDividendsClient<'static>) {
        let env = Env::default();
        env.ledger().with_mut(|li| li.sequence_number = 100);
        let contract_id = env.register_contract(None, TalosDividends);
        let client = TalosDividendsClient::new(&env, &contract_id);
        let admin = Address::generate(&env);

        client
            .mock_auths(&[MockAuth {
                address: &admin,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "initialize",
                    args: (admin.clone(),).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .initialize(&admin)
            .unwrap();

        (env, contract_id, admin, client)
    }

    fn commit(
        env: &Env,
        contract_id: &Address,
        client: &TalosDividendsClient<'static>,
        admin: &Address,
        revenue: i128,
        supply: i128,
    ) -> u32 {
        client
            .mock_auths(&[MockAuth {
                address: admin,
                invoke: &MockAuthInvoke {
                    contract: contract_id,
                    fn_name: "commit_epoch",
                    args: (admin.clone(), revenue, supply).into_val(env),
                    sub_invokes: &[],
                },
            }])
            .commit_epoch(admin, &revenue, &supply)
            .unwrap()
    }

    fn do_pause(
        env: &Env,
        contract_id: &Address,
        client: &TalosDividendsClient<'static>,
        admin: &Address,
        domain: Symbol,
    ) {
        client
            .mock_auths(&[MockAuth {
                address: admin,
                invoke: &MockAuthInvoke {
                    contract: contract_id,
                    fn_name: "pause",
                    args: (admin.clone(), domain.clone()).into_val(env),
                    sub_invokes: &[],
                },
            }])
            .pause(admin, &domain)
            .unwrap();
    }

    fn do_unpause(
        env: &Env,
        contract_id: &Address,
        client: &TalosDividendsClient<'static>,
        admin: &Address,
        domain: Symbol,
    ) {
        client
            .mock_auths(&[MockAuth {
                address: admin,
                invoke: &MockAuthInvoke {
                    contract: contract_id,
                    fn_name: "unpause",
                    args: (admin.clone(), domain.clone()).into_val(env),
                    sub_invokes: &[],
                },
            }])
            .unpause(admin, &domain)
            .unwrap();
    }

    // ── Basic functionality ────────────────────────────────────────────────────

    #[test]
    fn commit_epoch_and_claim_dividend_succeed_when_unpaused() {
        let (env, contract_id, admin, client) = setup();
        let patron = Address::generate(&env);

        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);

        let amount = client
            .mock_auths(&[MockAuth {
                address: &patron,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "claim_dividend",
                    args: (patron.clone(), epoch_id, 1_000_i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .claim_dividend(&patron, &epoch_id, &1_000_i128)
            .unwrap();

        // 1000 / 10000 * 1_000_000 = 100_000
        assert_eq!(amount, 100_000);
    }

    #[test]
    fn double_claim_returns_already_claimed() {
        let (env, contract_id, admin, client) = setup();
        let patron = Address::generate(&env);

        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);

        let claim = || {
            client
                .mock_auths(&[MockAuth {
                    address: &patron,
                    invoke: &MockAuthInvoke {
                        contract: &contract_id,
                        fn_name: "claim_dividend",
                        args: (patron.clone(), epoch_id, 500_i128).into_val(&env),
                        sub_invokes: &[],
                    },
                }])
                .claim_dividend(&patron, &epoch_id, &500_i128)
        };

        claim().unwrap();
        assert_eq!(claim(), Err(DividendError::AlreadyClaimed));
    }

    // ── Commit domain ─────────────────────────────────────────────────────────

    #[test]
    fn paused_commit_domain_blocks_commit_epoch() {
        let (env, contract_id, admin, client) = setup();
        let domain = Symbol::new(&env, DOMAIN_COMMIT);

        do_pause(&env, &contract_id, &client, &admin, domain.clone());

        let result = client
            .mock_auths(&[MockAuth {
                address: &admin,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "commit_epoch",
                    args: (admin.clone(), 500_000_i128, 5_000_i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .commit_epoch(&admin, &500_000_i128, &5_000_i128);

        assert_eq!(result, Err(DividendError::Paused));
    }

    #[test]
    fn unpausing_commit_domain_restores_commit_epoch() {
        let (env, contract_id, admin, client) = setup();
        let domain = Symbol::new(&env, DOMAIN_COMMIT);

        do_pause(&env, &contract_id, &client, &admin, domain.clone());
        do_unpause(&env, &contract_id, &client, &admin, domain);

        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);
        assert_eq!(epoch_id, 1);
    }

    // ── Claim domain ──────────────────────────────────────────────────────────

    #[test]
    fn paused_claim_domain_blocks_claim_dividend() {
        let (env, contract_id, admin, client) = setup();
        let patron = Address::generate(&env);
        let domain = Symbol::new(&env, DOMAIN_CLAIM);

        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);
        do_pause(&env, &contract_id, &client, &admin, domain.clone());

        let result = client
            .mock_auths(&[MockAuth {
                address: &patron,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "claim_dividend",
                    args: (patron.clone(), epoch_id, 1_000_i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .claim_dividend(&patron, &epoch_id, &1_000_i128);

        assert_eq!(result, Err(DividendError::Paused));
    }

    #[test]
    fn unpausing_claim_domain_restores_claim_dividend() {
        let (env, contract_id, admin, client) = setup();
        let patron = Address::generate(&env);
        let domain = Symbol::new(&env, DOMAIN_CLAIM);

        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);
        do_pause(&env, &contract_id, &client, &admin, domain.clone());
        do_unpause(&env, &contract_id, &client, &admin, domain);

        let amount = client
            .mock_auths(&[MockAuth {
                address: &patron,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "claim_dividend",
                    args: (patron.clone(), epoch_id, 1_000_i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .claim_dividend(&patron, &epoch_id, &1_000_i128)
            .unwrap();

        assert_eq!(amount, 100_000);
    }

    // ── Scoped isolation ──────────────────────────────────────────────────────

    #[test]
    fn pausing_commit_does_not_block_claim() {
        let (env, contract_id, admin, client) = setup();
        let patron = Address::generate(&env);

        // Commit first (before pausing commit domain)
        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);

        // Pause only the commit domain
        do_pause(
            &env,
            &contract_id,
            &client,
            &admin,
            Symbol::new(&env, DOMAIN_COMMIT),
        );

        // Claiming should still work
        let amount = client
            .mock_auths(&[MockAuth {
                address: &patron,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "claim_dividend",
                    args: (patron.clone(), epoch_id, 1_000_i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .claim_dividend(&patron, &epoch_id, &1_000_i128)
            .unwrap();

        assert_eq!(amount, 100_000);
    }

    #[test]
    fn pausing_claim_does_not_block_commit() {
        let (env, contract_id, admin, client) = setup();

        // Pause only the claim domain
        do_pause(
            &env,
            &contract_id,
            &client,
            &admin,
            Symbol::new(&env, DOMAIN_CLAIM),
        );

        // Committing should still work
        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);
        assert_eq!(epoch_id, 1);
    }

    // ── Recovery while paused ─────────────────────────────────────────────────

    #[test]
    fn recover_expired_works_while_commit_domain_is_paused() {
        let (env, contract_id, admin, client) = setup();

        // Commit while unpaused
        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);

        // Pause commit domain
        do_pause(
            &env,
            &contract_id,
            &client,
            &admin,
            Symbol::new(&env, DOMAIN_COMMIT),
        );

        // Recovery should still succeed even though commit is paused
        client
            .mock_auths(&[MockAuth {
                address: &admin,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "recover_expired",
                    args: (admin.clone(), epoch_id).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .recover_expired(&admin, &epoch_id)
            .unwrap();

        let epoch = client.get_epoch(&epoch_id).unwrap();
        assert!(epoch.expired, "epoch should be marked expired");
    }

    #[test]
    fn recover_expired_works_while_claim_domain_is_paused() {
        let (env, contract_id, admin, client) = setup();

        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);

        do_pause(
            &env,
            &contract_id,
            &client,
            &admin,
            Symbol::new(&env, DOMAIN_CLAIM),
        );

        client
            .mock_auths(&[MockAuth {
                address: &admin,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "recover_expired",
                    args: (admin.clone(), epoch_id).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .recover_expired(&admin, &epoch_id)
            .unwrap();

        let epoch = client.get_epoch(&epoch_id).unwrap();
        assert!(epoch.expired);
    }

    #[test]
    fn claiming_expired_epoch_returns_epoch_expired() {
        let (env, contract_id, admin, client) = setup();
        let patron = Address::generate(&env);

        let epoch_id = commit(&env, &contract_id, &client, &admin, 1_000_000, 10_000);

        client
            .mock_auths(&[MockAuth {
                address: &admin,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "recover_expired",
                    args: (admin.clone(), epoch_id).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .recover_expired(&admin, &epoch_id)
            .unwrap();

        let result = client
            .mock_auths(&[MockAuth {
                address: &patron,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "claim_dividend",
                    args: (patron.clone(), epoch_id, 1_000_i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .claim_dividend(&patron, &epoch_id, &1_000_i128);

        assert_eq!(result, Err(DividendError::EpochExpired));
    }

    // ── is_domain_paused query ────────────────────────────────────────────────

    #[test]
    fn is_domain_paused_reflects_state() {
        let (env, contract_id, admin, client) = setup();
        let domain = Symbol::new(&env, DOMAIN_CLAIM);

        assert!(!client.is_domain_paused(&domain));
        do_pause(&env, &contract_id, &client, &admin, domain.clone());
        assert!(client.is_domain_paused(&domain));
        do_unpause(&env, &contract_id, &client, &admin, domain.clone());
        assert!(!client.is_domain_paused(&domain));
    }

    // ── Unauthorized admin ────────────────────────────────────────────────────

    #[test]
    fn non_admin_cannot_pause() {
        let (env, contract_id, _admin, client) = setup();
        let attacker = Address::generate(&env);
        let domain = Symbol::new(&env, DOMAIN_CLAIM);

        let result = client
            .mock_auths(&[MockAuth {
                address: &attacker,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "pause",
                    args: (attacker.clone(), domain.clone()).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .pause(&attacker, &domain);

        assert_eq!(result, Err(DividendError::Unauthorized));
    }

    #[test]
    fn non_admin_cannot_unpause() {
        let (env, contract_id, admin, client) = setup();
        let attacker = Address::generate(&env);
        let domain = Symbol::new(&env, DOMAIN_CLAIM);

        do_pause(&env, &contract_id, &client, &admin, domain.clone());

        let result = client
            .mock_auths(&[MockAuth {
                address: &attacker,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "unpause",
                    args: (attacker.clone(), domain.clone()).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .unpause(&attacker, &domain);

        assert_eq!(result, Err(DividendError::Unauthorized));
    }
}
