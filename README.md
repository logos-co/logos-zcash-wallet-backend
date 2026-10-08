# logos-zcash-wallet-backend

`zcash_wallet_backend`: the coordinator between the Zcash wallet surfaces (the app, the CLI,
any module that asks for a payment) and the engine, `zcash_wallet_core_module`. It holds no
keys and caches no password.

## Roles

Two roles, each a set of module names. Both default to `zcash_wallet_ui`.

| Role | Methods |
|---|---|
| custodian | `configure`, `create_wallet`, `restore_wallet`, `open_wallet`, `change_password`, `reveal_seed`, `export_viewing_key`, `set_active_network`, `apply_preset`, `set_proxy` |
| approver | `approve_send`, `approve_migration` |
| either | `close_wallet`, `new_address`, `prepare_shielding`, `prepare_migration`, `pause_migration`, `resume_migration`, `cancel_migration` |
| any named module | `prepare_send`, and `cancel_send` for its own request |

`configure` replaces both roles at once and needs the custodian role. Roles are saved to
`roles.json` in the instance's persistence directory. To enrol the headless CLI before any
custodian exists, write that file by hand before loading the module:

```json
{"approvers": ["zcash_wallet_ui", "zcash_wallet_cli"], "custodians": ["zcash_wallet_ui", "zcash_wallet_cli"]}
```

A refused call answers exactly `{"ok":false,"error":"not authorized"}`.

## Sends

1. Any named module calls `prepare_send({ recipients: [{ address, amount, memo? }] })` (amount
   in zatoshis) or `prepare_send({ uri })` with a ZIP 321 request. One send is open per wallet.
2. The engine builds an unsigned preview: recipients, fee, change, pools spent, expiry, and
   the amount made public. `send_status` shows it, with the seconds left on its 120 s life.
3. An approver calls `approve_send(requestId, password)`. The engine proves, signs and
   broadcasts on a fresh Tor circuit; the other operator must see the transaction within two
   minutes, or it is sent there too.

A payment that needs funds from more than one pool is refused with `needs_mixed_pools`;
resending with `allowMixedPools: true` is the consent ZIP 315 asks for. Shielding
(`prepare_shielding(address)`, or every eligible address with an empty address) follows
the same review and approval.

## Everything else

Reads (`wallet_status`, `sync_status`, `balances`, `receive_info`, `history`,
`address_valid`, `servers`, `server_health`, `migration_status`) pass through to the engine
or the node module. Long operations return a backend job id; poll `job_status`. Events:
`wallet_state_changed`, `sync_progress`, `balance_changed`, `server_health_changed`,
`send_status_changed`, `migration_changed`, `job_finished`.

```bash
cd rust-lib && cargo test --no-default-features
```
