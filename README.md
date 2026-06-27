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
- **Automated deployment** - Smart deployment script that auto-creates accounts and handles ownership transfer

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
6. Transfers token ownership to controller
7. Verifies ownership
8. **Deletes both accounts and returns funds to signer**

This is ideal for testing the deployment flow without leaving accounts on testnet.

### Production Deployment (Mainnet)

For permanent deployment to mainnet:

```bash
# Deploy (automatically builds contracts first)
# Interactive mode - prompts for all configuration
just deploy

# Or specify network and signer
just deploy mainnet your-account.near

# Preview commands without executing
just deploy mainnet your-account.near --dry-run
```

**Interactive mode prompts:**
- Network selection (testnet/mainnet)
- Signer account (from credentials)
- Controller account ID
- Token account ID  
- Token metadata (name, symbol, decimals)
- Total supply
- Initial balance for new accounts (default: 10 NEAR)

**Production deployment flow:**
1. Checks if controller account exists, creates if needed
2. Checks if token account exists, creates if needed
3. Deploys controller contract
4. Deploys token contract with signer as initial owner
5. Transfers token ownership to controller
6. Verifies ownership

**After deployment, the controller owns the token and can:**
- Pause/unpause via `delegate_pause`
- Freeze/unfreeze accounts via `delegate_execution`
- Upgrade token via release management (`add_release_info`, `add_release_blob`, `upgrade`)

## How to Deploy Manually?

To deploy manually, install [`cargo-near`](https://github.com/near/cargo-near) and run:

```bash
# Create a new account
cargo near create-account <contract-account-id> --useFaucet

# Deploy the contract on it
cargo near deploy <contract-account-id>

# Initialize the contract
near call <contract-account-id> new '{"owner_id": "<contract-account-id>", "total_supply": "1000000000000000", "metadata": { "spec": "ft-1.0.0", "name": "Example Token Name", "symbol": "EXLT", "decimals": 8 }}' --accountId <contract-account-id>
```

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
- [near CLI](https://near.cli.rs) - Iteract with NEAR blockchain from command line
- [NEAR StackOverflow](https://stackoverflow.com/questions/tagged/nearprotocol)
- NEAR DevHub: [Telegram](https://t.me/neardevhub), [Twitter](https://twitter.com/neardevhub)
