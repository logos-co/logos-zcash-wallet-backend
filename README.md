# logos-zcash-wallet-backend

`zcash_wallet_backend`: the coordinator between the Zcash wallet surfaces (the app, the CLI,
any module that asks for a payment) and the engine, `zcash_wallet_core_module`. It holds no
keys and caches no password.

## Roles

Two roles, each a set of module names. Both default to `zcash_wallet_ui`.

| Role | Methods |
|---|---|
| custodian | `configure`, `create_wallet`, `restore_wallet`, `open_wallet`, `change_password`, `reveal_seed`, `export_viewing_key`, `set_active_network`, `apply_preset`, `set_proxy`, `set_servers`, `clear_suspect`, `watch_servers`, `set_local_node`, `set_chain_source`, `set_own_server`, `set_local_sends`, `set_tor_use` |
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

## Regtest (test harnesses)

With a `regtest.json` in this module's persistence directory, `list_networks` also offers
`regtest` and `set_active_network` takes it. The wallet core and `zcash_node_module` need the
same file in theirs. The node module's regtest routes (proxy `direct`, `http://127.0.0.1:PORT`
servers) reach the core unchanged.

## Routes

The backend hands the wallet core the node module's routes: the proxy, the sync and broadcast
servers, and `direct`, the servers the user set to skip Tor. Servers on the user's own network
need no proxy either. A route table that still needs a proxy, with none set, is refused.

With Tor for what identifies you, the route table also names the classes that go direct
(`directFor`: sync and tips), where lookups go, and who runs each server. The backend passes
them on as `directFor`, `details`, `taddr` and `operators` (url to operator name), and only
then, so a route table in either other mode reaches the core in the older shape.

The backend tells the node module whose servers to check (`watch`): the open wallet's
network's; with no wallet open, the active network's while the app shows them
(`watch_servers(true)`, which lasts 3 minutes unless renewed); otherwise none.

## Use Tor for

`tor_use()` returns the node module's `{ network, torUse, torUseChosen, torUseMigrated }` for the
active network: `all`, `identifying` or `none`. `set_tor_use(mode)` changes it for the active
network, and is refused with
"close the wallet to change this" while a wallet is open or an open, create or restore job is
running: routes are taken when a wallet opens, so a change would not reach it. The backend also
tells the node module which wallet is open (`set_wallet_open`), and the node keeps the same
lock.

## Where the chain comes from

Passed to the node module for the active network. `set_chain_source(source)`: `public` (the
server list, as Use Tor for says), `local` or `own`. Use Tor for applies to public servers only.

- `local`, also `set_local_node(true)`: every read goes to the node `zebrad_module` runs
  in-process, over Logos IPC. `set_local_sends("servers")`, the default, sends through the
  servers, and every request to them goes through Tor whatever Use Tor for says: sends, the
  check that follows one, the cross-check of the node's chain and the health checks.
  `set_local_sends("node")` has the node send transactions itself, over its own peer
  connections, and no server is contacted. `local_node()` reports whether it runs the active
  network.
- `own`: `set_own_server({ url, tor, label? })` saves the user's own lightwalletd and switches
  to it. Every read goes to it: directly on the user's network, through Tor for an onion
  service, and otherwise as `tor` says. Sends follow `set_local_sends` as for `local`: through
  the servers over Tor (the default), or through the user's server with `node`. A node module
  from before this sends through the user's server whatever it says.

The route table already says all of this, so the wallet core needs no change.

## Everything else

Reads (`wallet_status`, `sync_status`, `balances`, `receive_info`, `history`,
`address_valid`, `servers`, `server_health`, `local_node`, `tor_use`, `migration_status`) pass through to the engine
or the node module. Long operations return a backend job id; poll `job_status`. Events:
`wallet_state_changed`, `sync_progress`, `balance_changed`, `server_health_changed`,
`send_status_changed`, `migration_changed`, `job_finished`.

```bash
cd rust-lib && cargo test --no-default-features
```
