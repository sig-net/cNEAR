# Fungible Token (FT)

This is a fork of the NEAR example FT library.

This is a standard FT contract that additionally:
- Allows the owner to freeze/unfreeze accounts
- Allows the owner to force transfer out of accounts
- Integrates with aurora-controller-factory for upgrades and access control
- Uses near-plugins AccessControllable pattern for owner role management

## Features

- **Standard FT functionality** - Full NEP-141 fungible token implementation
- **Pausable** - Owner can pause/unpause all token transfers. Pausing (like freezing) only restricts non-owner methods
- **Account freezing** - Owner can freeze individual accounts to prevent their transfers, the owner's own methods (e.g. force transfer) are never blocked
- **Force transfers** - Owner can move tokens between any accounts, regardless of pause or freeze state
- **Upgradeable** - Owner can upgrade contract code
- **Controller integration** - Designed to work with aurora-controller-factory for DAO-controlled operations
- **Single owner** - exactly one account is the owner at any time, and `owner_get` is the only source of truth. Ownership transfer is two-step (propose, then the new owner accepts), so it can never be handed to an account that cannot act
- **Automated deployment** - Smart deployment script that auto-creates accounts and handles ownership transfer
- **No burn through storage_unregister** - unlike the NEP-145 reference behaviour, `storage_unregister` with `force: true` will not burn a positive balance. Transfer the balance out first, then unregister

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

### Testing Deployment (Testnet - Ephemeral)

Test the full deployment flow on testnet with temporary accounts that are automatically cleaned up:

```bash
# Test deployment (builds contracts, creates temporary accounts, deploys, then deletes them)
just deploy test

# Or specify a signer account
just deploy test your-account.testnet

# Preview commands without executing
just deploy test your-account.testnet --dry-run
```

**What happens in test mode:**
1. Prompts for signer account from `~/.near-credentials/testnet/`
2. Creates `controller.<signer>.testnet` subaccount (10 NEAR)
3. Creates `token.<signer>.testnet` subaccount (10 NEAR)
4. Deploys controller contract
5. Deploys token contract with signer as initial owner
6. Proposes the controller as the token's new owner
7. Has the controller accept ownership (via `delegate_execution`)
8. Verifies ownership
9. **Deletes both accounts and returns funds to signer**

This tests the deployment flow without leaving accounts on testnet.

### Production Deployment (Mainnet)

For permanent deployment to mainnet:

```bash
just deploy mainnet your-account.near
```

**Interactive mode prompts:**
- Network selection (testnet/mainnet)
- Signer account (from credentials)
- Controller account ID
- Token account ID  
- Token metadata (name, symbol, decimals)
- Total supply
- Initial price in yoctoNEAR (default: ONE_NEAR = 10^24, this implies 1 NEAR = 1 cNEAR)
- Initial balance for new accounts (default: 10 NEAR)

**Production deployment flow:**
1. Checks if controller account exists, creates if needed
2. Checks if token account exists, creates if needed
3. Deploys and initialises the controller contract in one transaction
4. Deploys and initialises the token contract with the signer as initial owner
5. Proposes the controller as the token's new owner
6. Has the controller accept ownership (via `delegate_execution`)
7. Verifies ownership through a view call

The deployment is implemented in Rust (`deploy-cli/`) and talks to the network through typed RPC calls rather than parsing the output of the `near` CLI. `scripts/deploy.sh` remains as a thin wrapper for anyone with it in muscle memory. Credentials come from `NEAR_CREDENTIALS/<network>/` (or `~/.near-credentials/<network>/`).

**After deployment, the controller owns the token and can:**
- Pause/unpause via `delegate_pause`
- Freeze/unfreeze accounts via `delegate_execution`
- Upgrade token via release management (`add_release_info`, `add_release_blob`, `upgrade`)

**We reccomend that the token account is funded with at least 60 NEAR, and the controller account with at least 20 NEAR.** The controller stores its own code (~4.1 NEAR) and a copy of each token release blob it is given (~3.4 NEAR per release), so it needs headroom of its own. The contract code itself locks ~3.4 NEAR of storage, and the freeze list is funded from the contract's own balance rather than by callers. Each 1 NEAR gives you to have ~900 frozen accounts, allowing you to freeze 5,000 accounts before you have to top up.

Once you are happy with a deployment, and have manually tested all the functionality, you must remove the access keys by running:

```bash
cargo run -p cnear-deploy -- finalize mainnet <signer-account-id>
```

A keyless contract whose upgrade path does not work cannot be repaired by anyone, so only run this after the release, pause, freeze and upgrade checks have all passed. 

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
near call <contract-account-id> pause_contract --accountId <owner-account-id>

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

# Transfer ownership - step 1: the current owner proposes (owner only).
# This does NOT move ownership yet.
near call <contract-account-id> owner_set '{"new_owner": "<new-owner-account-id>"}' --accountId <owner-account-id>

# Transfer ownership - step 2: the proposed account accepts, completing the
# transfer. Only that account can call this.
near call <contract-account-id> owner_accept --accountId <new-owner-account-id>

# Inspect or abandon a pending transfer
near view <contract-account-id> pending_owner_get
near call <contract-account-id> owner_cancel_transfer --accountId <owner-account-id>
```

## Controller integration

When deployed with aurora-controller-factory as owner:

```bash
# Pause via controller
near call <controller-id> delegate_pause '{"receiver_id": "<token-id>", "pause_method_name": "pause_contract"}' --accountId <dao-account> --amount 0.000000000000000000000001

# Freeze via controller  
near call <controller-id> delegate_execution '{"receiver_id": "<token-id>", "actions": [{"function_name": "freeze_account", "arguments": "<base64-encoded-args>", "amount": "0", "gas": "5000000000000"}]}' --accountId <dao-account> --amount 0.000000000000000000000001

# Accept ownership via controller. Needed once, after the token proposes the
# controller as its new owner: ownership only moves when the controller itself
# accepts, which proves it can act on the token.
near call <controller-id> delegate_execution '{"receiver_id": "<token-id>", "actions": [{"function_name": "owner_accept", "arguments": "", "amount": "0", "gas": "20000000000000"}]}' --accountId <dao-account> --amount 0.000000000000000000000001

# Upgrade via controller
# 1. Add release info
near call <controller-id> add_release_info '{"hash": "<wasm-sha256>", "version": "1.1.0", "is_latest": true}' --accountId <dao-account> --amount 0.000000000000000000000001

# 2. Upload wasm blob
near call <controller-id> add_release_blob --accountId <dao-account> --amount 0.000000000000000000000001 < token.wasm

# 3. Register the deployment, if the controller did not deploy the token itself.
# Without this record the controller rejects every upgrade.
near call <controller-id> add_deployment_info '{"contract_id": "<token-id>", "deployment_info": {"hash": "<wasm-sha256>", "version": "1.0.0", "deployment_time": 0, "upgrade_times": {}, "init_args": ""}}' --accountId <dao-account> --amount 0.000000000000000000000001

# 4. Upgrade token
near call <controller-id> upgrade '{"contract_id": "<token-id>", "hash": "<wasm-sha256>"}' --accountId <dao-account> --amount 0.000000000000000000000001

# 5. Verify the upgraded contract actually runs. A successful controller
# transaction does NOT prove the token contains executable code.
near view <token-id> owner_get
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
- [near CLI](https://near.cli.rs) - Iteract with NEAR blockchain from command line
- [NEAR StackOverflow](https://stackoverflow.com/questions/tagged/nearprotocol)
- NEAR DevHub: [Telegram](https://t.me/neardevhub), [Twitter](https://twitter.com/neardevhub)
