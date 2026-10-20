# Soroban Contract Events

This document lists every event emitted by the Talos Protocol Soroban contracts.
All events follow the Soroban SDK convention: `env.events().publish(topics, data)`.

Topics are a tuple; the first element is always a `Symbol` identifying the event type.
Data is a tuple containing the payload fields.

---

## TalosNameService

Contract address (testnet): `CAP32WBDUMF4UOXLSCHQDM3MDKS4WNQUQAB4WMAGUXTXJNQ4OWD3CQXF`

### `name_reg` — Name registered

Emitted when a Talos successfully registers (or renames to) a human-readable name.

| | |
|---|---|
| **Symbol** | `name_reg` |

**Topics**

| Index | Type | Description |
|-------|------|-------------|
| 0 | `Symbol` | `"name_reg"` |
| 1 | `u32` | `talos_id` — the on-chain Talos ID that received the name |

**Data** `(String, Address)`

| Index | Type | Description |
|-------|------|-------------|
| 0 | `String` | `name` — the registered name (e.g. `"marketbot"`) |
| 1 | `Address` | `owner` — the address that authorised the registration |

**Example (XDR-decoded)**
```
topics: ["name_reg", 42]
data:   ["marketbot", "GCREATOR..."]
```

---

### `name_rel` — Name released

Emitted when a Talos creator releases a registered name, freeing it for re-registration.
Both the forward mapping (`name → talos_id`) and the reverse mapping (`talos_id → name`)
are removed from persistent storage before this event is published.

| | |
|---|---|
| **Symbol** | `name_rel` |

**Topics**

| Index | Type | Description |
|-------|------|-------------|
| 0 | `Symbol` | `"name_rel"` |
| 1 | `u32` | `talos_id` — the Talos whose name was released |

**Data** `(String, Address)`

| Index | Type | Description |
|-------|------|-------------|
| 0 | `String` | `name` — the name that was released |
| 1 | `Address` | `owner` — the address that authorised the release (must be the registry creator) |

**Example (XDR-decoded)**
```
topics: ["name_rel", 42]
data:   ["marketbot", "GCREATOR..."]
```

**Entry-point**: `release_name(owner: Address, talos_id: u32)`  
**Authorization**: `owner.require_auth()` + cross-contract `creator_of(talos_id)` check against TalosRegistry.  
**Panics if**: `talos_id` has no registered name, or `owner` is not the registry creator.

---

### `reg_upd` — Registry contract updated

Emitted when the TalosRegistry contract pointer is changed (directly or via timelock).

| | |
|---|---|
| **Symbol** | `reg_upd` |

**Topics**

| Index | Type | Description |
|-------|------|-------------|
| 0 | `Symbol` | `"reg_upd"` |

**Data** `(Address, Address)`

| Index | Type | Description |
|-------|------|-------------|
| 0 | `Address` | `old_registry` |
| 1 | `Address` | `new_registry` |

---

### `tl_sch` — Timelock action scheduled

| | |
|---|---|
| **Symbol** | `tl_sch` |

**Topics**: `("tl_sch", proposal_id: u64)`  
**Data**: `(action: AdminAction, eta: u64, proposer: Address)`

---

### `tl_exec` — Timelock action executed

| | |
|---|---|
| **Symbol** | `tl_exec` |

**Topics**: `("tl_exec", proposal_id: u64)`  
**Data**: `(action: AdminAction, executor: Address)`

---

### `tl_cnl` — Timelock action cancelled

| | |
|---|---|
| **Symbol** | `tl_cnl` |

**Topics**: `("tl_cnl", proposal_id: u64)`  
**Data**: `(action: AdminAction, canceller: Address)`

---

### `tl_cfg` — Timelock configuration changed

| | |
|---|---|
| **Symbol** | `tl_cfg` |

**Topics**: `("tl_cfg",)`  
**Data**: `(old_min_delay: u64, new_min_delay: u64, grace_period: u64)`

---

## TalosRegistry

Contract address (testnet): `CAMLL62EVCRV5CEHBO34MHUAHI5FJE5XFVAU57VZEAVY2EE5Q36FVHUX`

### `tls_crt` — Talos created

Emitted when a new Talos is registered on-chain.

**Topics**: `("tls_crt", talos_id: u32)`  
**Data**: `(name: String, category: String, creator: Address)`

---

### `pat_upd` — Patron configuration updated

**Topics**: `("pat_upd", talos_id: u32)`  
**Data**: `(creator_share: u32, investor_share: u32, treasury_share: u32)`

---

### `fee_chg` — Protocol fee changed

**Topics**: `("fee_chg",)`  
**Data**: `(old_fee_bps: u32, new_fee_bps: u32)`

---

### `adm_prp` — Admin transfer proposed

**Topics**: `("adm_prp",)`  
**Data**: `(current_admin: Address, proposed_admin: Address)`

---

### `adm_acc` — Admin transfer accepted

**Topics**: `("adm_acc",)`  
**Data**: `(new_admin: Address)`

---

### `adm_cnl` — Admin transfer cancelled

**Topics**: `("adm_cnl",)`  
**Data**: `(cancelled_by: Address)`

---

### Timelock events (`tl_sch`, `tl_exec`, `tl_cnl`, `tl_cfg`)

Same schema as TalosNameService timelock events above.

---

## TalosGovernance

### `proposal_created`

**Topics**: `("proposal_created", proposal_id: u64)`  
**Data**: `(talos_id: u32, proposer: Address, title: String)`

---

### `vote_cast`

**Topics**: `("vote_cast", proposal_id: u64)`  
**Data**: `(voter: Address, weight: u64, in_favour: bool)`

---

### `proposal_status_changed`

**Topics**: `("proposal_status_changed", proposal_id: u64)`  
**Data**: `(old_status: Symbol, new_status: Symbol)`

---

## Subscribing to events

Use the Stellar Horizon events endpoint or the Soroban RPC `getEvents` method:

```bash
stellar events \
  --network testnet \
  --contract-id CAP32WBDUMF4UOXLSCHQDM3MDKS4WNQUQAB4WMAGUXTXJNQ4OWD3CQXF \
  --type contract \
  --start-ledger <ledger>
```

Filter by topic symbol to isolate a specific event type:

```bash
# Only name release events
stellar events ... --filter-topics name_rel
```
