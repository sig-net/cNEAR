# Fungible Token (FT)

This is a fork of the NEAR example FT library.

This is a standard FT contract that additionally:
- Allows the owner to freeze/unfreeze accounts
- Allows the owner to force transfer out of accounts
- Integrates with aurora-controller-factory for upgrades and access control
- Uses near-plugins AccessControllable pattern for owner role management

## Features

- **Standard FT functionality** - Full NEP-141 fungible token implementation
- **Pausable** - Owner can pause/unpause all token transfers
- **Account freezing** - Owner can freeze individual accounts to prevent their transfers
- **Force transfers** - Owner can move tokens between any accounts
- **Upgradeable** - Owner can upgrade contract code
- **Controller integration** - Designed to work with aurora-controller-factory for DAO-controlled operations
- **Access control** - Role-based permissions using near-plugins AccessControllable
- **Automated deployment** - Typed Rust deployer that validates credentials, creates accounts, handles ownership transfer, and verifies final state

## How to Build Locally?

Install [`cargo-near`](https://github.com/near/cargo-near) and run:

```bash
# Build token contract
just build-token

# Build controller contract
just build-controller

# Build both
just build
```

## How to Test Locally?

```bash
# Run unit tests
just test-unit

# Run integration tests (requires built wasms)
just test-integration

# Run all tests
just test
```

## How to Deploy?

### Typed Deployment

Deployment is performed by `cnear-deploy`, a Rust binary that uses `near-api` 0.8.6 for typed transaction construction, local signing, and single-attempt broadcast, and uses typed NEAR JSON-RPC queries for preflight inspection and hash-based reconciliation. It never invokes the `near` CLI. The deployer validates account IDs and numeric values, checks WASM magic bytes and prints SHA-256 hashes, queries the signer access key and final block hash before every transaction, and requires final execution success. It maintains a local next-nonce tracker for each signing account/key, so sequential transactions do not rely on a lagging replica returning the latest nonce; the tracker advances only after confirmation and never moves backwards on stale RPC data. After signing, it records the transaction hash before broadcast; a transport timeout or RPC timeout is treated as ambiguous, not failed. It polls transaction status using that hash and signer account, refreshes nonce state after an ambiguous submission, and never immediately resubmits the actions, preventing duplicate execution and nonce errors. If status remains unresolved, the error includes the hash so it can be checked later.

Credentials must be standard NEAR JSON files containing `account_id`, `public_key`, and `private_key`. The selected file must be a regular, non-symlink file with mode `0600`; its public and private keys must match, and the account/key must be an on-chain full-access key. Set `NEAR_CREDENTIALS` to the credentials root or use `--credentials PATH`.

For an ephemeral testnet deployment, accounts created by this invocation are deleted in reverse dependency order even when deployment fails. Existing accounts are never deleted. Generated key files are stored with mode `0600` and removed after successful test cleanup.

```bash
# Build contracts and deploy temporary testnet accounts
just deploy-test

# For signer selection and all other typed options, invoke the binary directly
cargo run --manifest-path deploy/Cargo.toml -- --network testnet --test-mode \
  --signer-id your-account.testnet \
  --credentials "$HOME/.near-credentials/testnet/your-account.testnet.json"

# Validate inputs and WASM without submitting transactions
cargo run --manifest-path deploy/Cargo.toml -- --network testnet --test-mode --dry-run \
  --signer-id your-account.testnet \
  --credentials "$HOME/.near-credentials/testnet/your-account.testnet.json"
```

For permanent deployment, specify the network, signer, and credentials explicitly. Mainnet requires typing `mainnet` unless `--yes` is provided deliberately:

```bash
just deploy-mainnet

# Or pass the complete typed configuration directly
cargo run --manifest-path deploy/Cargo.toml -- --network mainnet \
  --signer-id your-account.near \
  --credentials "$HOME/.near-credentials/mainnet/your-account.near.json"
```

Controller and token IDs default to `controller.<signer>` and `token.<signer>`. Use `--controller-id` and `--token-id` to override them. Existing accounts with deployed code are rejected by default; pass `--redeploy` only when replacing an intentional deployment. Accounts that already contain contract state are always rejected because the deployer will not automatically reinitialize stateful contracts.

**Amount flags:** `--total-supply`, `--initial-price`, and `--initial-balance` accept decimal amounts with a `near-token` unit, parsed by the `near-token` crate (the same format as `NearToken::from_str`). Units are case-insensitive, and whitespace between the amount and the unit is optional: `NEAR`/`N` (1 NEAR = 10^24 yoctoNEAR), `MILLINEAR` (10^21), `MICRONEAR` (10^18), or `YN`/`YNEAR`/`YOCTONEAR` (1). Examples: `--total-supply "1000000000000000 YN"`, `--initial-price "1 NEAR"`, `--initial-balance "0.5 N"`. Defaults preserve the legacy raw-yocto values: `--total-supply` defaults to `0.000000001 NEAR` (1,000,000,000,000,000 yocto), `--initial-price` to `1 NEAR`, and `--initial-balance` to `10 NEAR`. Each value is converted to a raw yoctoNEAR decimal string for the contract initialization arguments.

**Deployment flow:**
1. Validate credentials, WASM files, account IDs, and numeric configuration.
2. Verify signer access and inspect target account state through typed RPC responses.
3. Create missing accounts with newly generated full-access keys.
4. Deploy and initialize the controller and token.
5. Transfer token ownership to the controller.
6. Verify `owner_get` using typed JSON parsing.
7. In `--test-mode`, delete newly-created token then controller accounts and return funds to the signer.

**Ambiguous submissions:** A timeout after broadcast does not prove failure. `near-api` performs exactly one broadcast attempt against the configured endpoint; the deployer retains the exact signed transaction hash, then uses typed JSON-RPC status queries (`EXPERIMENTAL_tx_status`) until success/failure or a bounded unresolved timeout. It refuses to submit a duplicate transaction while status is unknown. Save the reported hash when an unresolved error occurs and inspect it before taking any manual recovery action.

**After deployment, the controller owns the token and can:**
- Pause/unpause via `delegate_pause`
- Freeze/unfreeze accounts via `delegate_execution`
- Upgrade token via release management (`add_release_info`, `add_release_blob`, `upgrade`)

The deployer does not log private keys. Keep credential files backed up securely; deleting a generated key file after a non-test deployment would make the account inaccessible. The contract interaction examples below use the NEAR CLI only for post-deployment calls and views, not for deployment.
## Basic methods
```bash
# View metadata
near view <contract-account-id> ft_metadata

# View owner
near view <contract-account-id> owner_get

# Make a storage deposit
near call <contract-account-id> storage_deposit '' --accountId <account-id> --amount 0.00125

# View balance
near view <contract-account-id> ft_balance_of '{"account_id": "<account-id>"}'

# View latest price
near view <contract-account-id> get_latest_price

# Transfer tokens
near call <contract-account-id> ft_transfer '{"receiver_id": "<account-id>", "amount": "19"}' --accountId <contract-account-id> --amount 0.000000000000000000000001
```

## Owner-only methods

```bash
# Pause contract (owner only)
near call <contract-account-id> pause --accountId <owner-account-id>

# Unpause contract (owner only)
near call <contract-account-id> unpause --accountId <owner-account-id>

# Freeze account (owner only)
near call <contract-account-id> freeze_account '{"account_id": "<target-account-id>"}' --accountId <owner-account-id>

# Unfreeze account (owner only)
near call <contract-account-id> unfreeze_account '{"account_id": "<target-account-id>"}' --accountId <owner-account-id>

# Check if account is frozen
near view <contract-account-id> is_frozen '{"account_id": "<target-account-id>"}'

# Force transfer (owner only)
near call <contract-account-id> force_ft_transfer '{"sender_id": "<from-account>", "receiver_id": "<to-account>", "amount": "1000"}' --accountId <owner-account-id> --amount 0.000000000000000000000001

# Set latest price (owner only)
near call <contract-account-id> set_latest_price '{"price": "1000000000000000000000000"}' --accountId <owner-account-id>

# Transfer ownership (owner only)
near call <contract-account-id> owner_set '{"new_owner": "<new-owner-account-id>"}' --accountId <owner-account-id>
```

## Controller integration

When deployed with aurora-controller-factory as owner:

```bash
# Pause via controller
near call <controller-id> delegate_pause '{"receiver_id": "<token-id>", "pause_method_name": "pause_contract"}' --accountId <dao-account> --amount 0.000000000000000000000001

# Freeze via controller  
near call <controller-id> delegate_execution '{"receiver_id": "<token-id>", "actions": [{"function_name": "freeze_account", "arguments": "<base64-encoded-args>", "amount": "0", "gas": "5000000000000"}]}' --accountId <dao-account> --amount 0.000000000000000000000001

# Upgrade via controller
# 1. Add release info
near call <controller-id> add_release_info '{"hash": "<wasm-sha256>", "version": "1.1.0", "is_latest": true}' --accountId <dao-account> --amount 0.000000000000000000000001

# 2. Upload wasm blob
near call <controller-id> add_release_blob --accountId <dao-account> --amount 0.000000000000000000000001 < token.wasm

# 3. Upgrade token
near call <controller-id> upgrade '{"contract_id": "<token-id>", "hash": "<wasm-sha256>"}' --accountId <dao-account> --amount 0.000000000000000000000001
```

## Notes

 - The maximum balance value is limited by U128 (`2**128 - 1`).
 - JSON calls should pass U128 as a base-10 string. E.g. "100".
 - This does not include escrow functionality, as `ft_transfer_call` provides a superior approach. An escrow system can, of course, be added as a separate contract or additional functionality within this contract.

## Useful Links

- [NEAR Documentation](https://docs.near.org)
- [NEAR Telegram Developers Community Group](https://t.me/neardev)

- [Smart Contracts Docs](https://docs.near.org/smart-contracts/anatomy)
- [cargo-near](https://github.com/near/cargo-near) - NEAR smart contract development toolkit for Rust
- [near CLI](https://near.cli.rs) - Interact with NEAR after deployment
- [NEAR StackOverflow](https://stackoverflow.com/questions/tagged/nearprotocol)
- NEAR DevHub: [Telegram](https://t.me/neardevhub), [Twitter](https://twitter.com/neardevhub)
