//! pause_control — Scoped pause/unpause for Talos Protocol Soroban contracts.
//!
//! ## Overview
//!
//! This crate provides a lightweight, reusable pause mechanism that can be
//! composed into any Soroban contract.  Instead of a single global pause flag,
//! it uses **named domains** so operators can freeze only the affected
//! functionality (e.g. `"commit"` or `"claim"`) without halting the rest of
//! the contract.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use pause_control::{check_not_paused, pause_domain, unpause_domain, PauseError};
//!
//! // In an entry point that should be pausable:
//! pub fn commit_epoch(env: Env, ...) {
//!     check_not_paused(&env, DOMAIN_COMMIT).unwrap_or_else(|_| panic!("Paused: commit"));
//!     // ... rest of logic
//! }
//!
//! // Admin-only pause:
//! pub fn pause(env: Env, admin: Address, domain: Symbol) {
//!     admin.require_auth();
//!     pause_domain(&env, domain);
//! }
//!
//! // Admin-only unpause:
//! pub fn unpause(env: Env, admin: Address, domain: Symbol) {
//!     admin.require_auth();
//!     unpause_domain(&env, domain);
//! }
//! ```
//!
//! ## Storage layout
//!
//! Each domain is stored under `DataKey::Paused(domain_symbol)` in the
//! contract's **temporary** storage.  Using temporary storage means pause
//! entries expire automatically with the ledger TTL, providing an implicit
//! safety valve.  The consuming contract never needs to call `unpause_domain`
//! to clean up — entries expire on their own.
//!
//! ## Events
//!
//! | Event       | Topics                        | Data              | Emitted on    |
//! |-------------|-------------------------------|-------------------|---------------|
//! | `pc_pause`  | `(symbol_short!("pc_pause"),)`| `(domain: Symbol)`| `pause_domain`|
//! | `pc_unpause`| `(symbol_short!("pc_unpause"),)`| `(domain: Symbol)`| `unpause_domain`|

#![no_std]

#[cfg(all(test, not(target_arch = "wasm32")))]
extern crate std;

use soroban_sdk::{contracttype, symbol_short, Env, Symbol};

// ── Error ────────────────────────────────────────────────────────────────────

/// Returned by [`check_not_paused`] when the domain is paused.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PauseError {
    /// The requested operation domain is currently paused.
    DomainPaused,
}

// ── Storage key ──────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PauseKey {
    Paused(Symbol),
}

// ── Core functions ───────────────────────────────────────────────────────────

/// Return `Err(PauseError::DomainPaused)` when the given *domain* is paused,
/// or `Ok(())` when it is active.
///
/// Call this at the top of every entry point that should be pausable.
pub fn check_not_paused(env: &Env, domain: Symbol) -> Result<(), PauseError> {
    let key = PauseKey::Paused(domain);
    if env.storage().temporary().has(&key) {
        Err(PauseError::DomainPaused)
    } else {
        Ok(())
    }
}

/// Mark *domain* as paused.  Emits a `pc_pause` event.
///
/// This function is intentionally **not** admin-gated — the calling contract
/// is responsible for performing any authorization check before invoking it.
pub fn pause_domain(env: &Env, domain: Symbol) {
    let key = PauseKey::Paused(domain.clone());
    env.storage().temporary().set(&key, &true);
    env.events()
        .publish((symbol_short!("pc_pause"),), (domain,));
}

/// Remove the pause flag for *domain*.  Emits a `pc_unpause` event.
///
/// This function is intentionally **not** admin-gated — the calling contract
/// is responsible for performing any authorization check before invoking it.
pub fn unpause_domain(env: &Env, domain: Symbol) {
    let key = PauseKey::Paused(domain.clone());
    env.storage().temporary().remove(&key);
    env.events()
        .publish((symbol_short!("pc_unpause"),), (domain,));
}

/// Return `true` when *domain* is currently paused.
pub fn is_paused(env: &Env, domain: Symbol) -> bool {
    env.storage()
        .temporary()
        .has(&PauseKey::Paused(domain))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short, Env};

    fn env() -> Env {
        Env::default()
    }

    #[test]
    fn check_not_paused_passes_when_domain_is_active() {
        let env = env();
        let domain = symbol_short!("commit");
        assert!(check_not_paused(&env, domain).is_ok());
    }

    #[test]
    fn check_not_paused_fails_after_pause_domain() {
        let env = env();
        let domain = symbol_short!("claim");
        pause_domain(&env, domain.clone());
        assert_eq!(
            check_not_paused(&env, domain),
            Err(PauseError::DomainPaused)
        );
    }

    #[test]
    fn unpause_domain_restores_normal_operation() {
        let env = env();
        let domain = symbol_short!("commit");
        pause_domain(&env, domain.clone());
        unpause_domain(&env, domain.clone());
        assert!(check_not_paused(&env, domain).is_ok());
    }

    #[test]
    fn is_paused_reflects_current_state() {
        let env = env();
        let domain = symbol_short!("claim");
        assert!(!is_paused(&env, domain.clone()));
        pause_domain(&env, domain.clone());
        assert!(is_paused(&env, domain.clone()));
        unpause_domain(&env, domain.clone());
        assert!(!is_paused(&env, domain));
    }

    #[test]
    fn pausing_one_domain_does_not_affect_another() {
        let env = env();
        let commit = symbol_short!("commit");
        let claim = symbol_short!("claim");
        pause_domain(&env, commit.clone());
        assert!(check_not_paused(&env, claim).is_ok());
        assert_eq!(
            check_not_paused(&env, commit),
            Err(PauseError::DomainPaused)
        );
    }

    #[test]
    fn unpause_nonexistent_domain_is_safe() {
        let env = env();
        let domain = symbol_short!("noexist");
        // Should not panic
        unpause_domain(&env, domain.clone());
        assert!(check_not_paused(&env, domain).is_ok());
    }
}
