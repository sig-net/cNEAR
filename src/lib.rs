/*!
Fungible Token implementation with JSON serialization.
NOTES:
  - The maximum balance value is limited by U128 (2**128 - 1).
  - JSON calls should pass U128 as a base-10 string. E.g. "100".
  - The contract optimizes the inner trie structure by hashing account IDs. It will prevent some
    abuse of deep tries. Shouldn't be an issue, once NEAR clients implement full hashing of keys.
  - For the standard NEP-141 storage-management methods, the contract tracks the change in storage
    before and after the call. If the storage increases,
    the contract requires the caller of the contract to attach enough deposit to the function call
    to cover the storage cost.
    This is done to prevent a denial of service attack on the contract by taking all available storage.
    If the storage decreases, the contract will issue a refund for the cost of the released storage.
    The unused tokens from the attached deposit are also refunded, so it's safe to
    attach more deposit than required.
  - The cNEAR-specific freeze list is the exception: `freeze_account` is not payable and its storage
    is funded from the contract account's own balance, not by the caller. `unfreeze_account` releases
    that stake back to the contract and refunds nobody. The freeze list is therefore bounded by the
    contract's available NEAR balance: if it runs out, further freezes fail until the account is
    topped up. Deploy with a reserve and monitor it — see "Errata" in README.md for the numbers.
  - To prevent the deployed contract from being modified or deleted, it should not have any access
    keys on its account.
*/
use near_contract_standards::fungible_token::metadata::{
    FungibleTokenMetadata, FungibleTokenMetadataProvider,
};
use near_contract_standards::fungible_token::{
    FungibleToken, FungibleTokenCore, FungibleTokenResolver,
};
use near_contract_standards::storage_management::{
    StorageBalance, StorageBalanceBounds, StorageManagement,
};
use near_plugins::{access_control, access_control_any, AccessControlRole, AccessControllable};
use near_sdk::borsh::BorshSerialize;
use near_sdk::collections::LazyOption;
use near_sdk::json_types::U128;
use near_sdk::store::LookupSet;
use near_sdk::{
    assert_one_yocto, env, log, near, require, AccountId, BorshStorageKey, NearToken,
    PanicOnDefault, Promise, PromiseOrValue,
};

#[derive(AccessControlRole, Clone, Copy)]
#[near(serializers = [json])]
enum Role {
    Owner,
}

#[derive(PanicOnDefault)]
#[access_control(role_type(Role))]
#[near(contract_state)]
pub struct Contract {
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
    frozen_accounts: LookupSet<AccountId>,
    paused: bool,
    owner_id: AccountId,
    latest_price: u128,
}

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum StorageKey {
    FungibleToken,
    Metadata,
    FrozenAccounts,
}

/// NEP-297 events for the cNEAR-specific administrative operations, so that off-chain monitoring can consume from an event interface instead of parsing transactions or polling state.
///
/// Every successful invocation of a privileged method emits its event, even when the call did not change state (e.g. pausing an already-paused contract).
#[near(event_json(standard = "cnear"))]
pub enum ContractEvent {
    #[event_version("1.0.0")]
    Paused { by: AccountId },
    #[event_version("1.0.0")]
    Unpaused { by: AccountId },
    #[event_version("1.0.0")]
    AccountFrozen {
        by: AccountId,
        account_id: AccountId,
    },
    #[event_version("1.0.0")]
    AccountUnfrozen {
        by: AccountId,
        account_id: AccountId,
    },
    #[event_version("1.0.0")]
    PriceUpdated {
        by: AccountId,
        old_price: U128,
        new_price: U128,
    },
    #[event_version("1.0.0")]
    OwnerChanged {
        old_owner: AccountId,
        new_owner: AccountId,
    },
    #[event_version("1.0.0")]
    ForceFtTransfer {
        by: AccountId,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
    },
}

#[near]
impl Contract {
    /// Initializes the contract with the given total supply owned by the given `owner_id` with the given fungible token metadata.
    #[init]
    pub fn new(
        owner_id: AccountId,
        total_supply: U128,
        metadata: FungibleTokenMetadata,
        latest_price: U128,
    ) -> Self {
        require!(!env::state_exists(), "Already initialized");
        metadata.assert_valid();
        let mut this = Self {
            token: FungibleToken::new(StorageKey::FungibleToken),
            metadata: LazyOption::new(StorageKey::Metadata, Some(&metadata)),
            frozen_accounts: LookupSet::new(StorageKey::FrozenAccounts),
            paused: false,
            owner_id: owner_id.clone(),
            latest_price: latest_price.into(),
        };

        // Grant Owner role to owner_id
        this.grant_owner_role(&owner_id);

        this.token.internal_register_account(&owner_id);
        this.token.internal_deposit(&owner_id, total_supply.into());

        near_contract_standards::fungible_token::events::FtMint {
            owner_id: &owner_id,
            amount: total_supply,
            memo: Some("new tokens are minted"),
        }
        .emit();

        this
    }

    /// Pause all *non-owner* token transfers (`ft_transfer`, `ft_transfer_call`). Pausing does not restrict owner-only methods such as [`Self::force_ft_transfer`] since the owner can always unpause.
    /// Emits a `Paused` event on every successful call, even if already paused.
    #[access_control_any(roles(Role::Owner))]
    pub fn pause(&mut self) {
        self.paused = true;
        ContractEvent::Paused {
            by: env::predecessor_account_id(),
        }
        .emit();
    }

    /// Alias for pause() - matches Aurora Controller expected method name
    #[access_control_any(roles(Role::Owner))]
    pub fn pause_contract(&mut self) {
        self.pause();
    }

    /// Emits an `Unpaused` event on every successful call, even if not paused.
    #[access_control_any(roles(Role::Owner))]
    pub fn unpause(&mut self) {
        self.paused = false;
        ContractEvent::Unpaused {
            by: env::predecessor_account_id(),
        }
        .emit();
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Upgrade contract code - owner only.
    ///
    /// The arguments are Borsh-encoded, matching the interface the Aurora
    /// controller calls (`code`, `state_migration_gas`).
    #[access_control_any(roles(Role::Owner))]
    pub fn upgrade(
        &self,
        #[serializer(borsh)] code: Vec<u8>,
        #[serializer(borsh)] state_migration_gas: Option<u64>,
    ) -> Promise {
        // The controller attaches 1 yoctoNEAR; requiring it keeps function-call
        // access keys (which cannot attach a deposit) away from this method.
        assert_one_yocto();
        require!(
            state_migration_gas.is_none(),
            "state migration is not supported: state_migration_gas must be null"
        );
        // Reject anything that is not a WASM module, so a malformed upgrade
        // fails here instead of installing code the runtime cannot compile.
        require!(
            code.starts_with(b"\0asm"),
            "code is not a WASM module (expected the \\0asm magic bytes)"
        );
        Promise::new(env::current_account_id()).deploy_contract(code)
    }

    /// Freeze an account, blocking it from *non-owner* transfer methods. Like [`Self::pause`], the owner can still move the frozen account's funds via [`Self::force_ft_transfer`]
    /// Emits an `AccountFrozen` event on every successful call, even if already frozen.
    ///
    /// Each newly frozen account locks ~0.0005–0.0011 NEAR of the *contract's* balance for
    /// storage (the caller pays nothing). Freezes fail once that balance is exhausted, so keep
    /// the contract funded — see "Errata" in README.md.
    #[access_control_any(roles(Role::Owner))]
    pub fn freeze_account(&mut self, account_id: AccountId) {
        self.frozen_accounts.insert(account_id.clone());
        ContractEvent::AccountFrozen {
            by: env::predecessor_account_id(),
            account_id,
        }
        .emit();
    }

    /// Emits an `AccountUnfrozen` event on every successful call, even if not frozen.
    #[access_control_any(roles(Role::Owner))]
    pub fn unfreeze_account(&mut self, account_id: AccountId) {
        self.frozen_accounts.remove(&account_id);
        ContractEvent::AccountUnfrozen {
            by: env::predecessor_account_id(),
            account_id,
        }
        .emit();
    }

    pub fn is_frozen(&self, account_id: AccountId) -> bool {
        self.frozen_accounts.contains(&account_id)
    }

    /// Emits a `PriceUpdated` event on every publication, including the previous and new values
    #[access_control_any(roles(Role::Owner))]
    pub fn set_latest_price(&mut self, price: U128) {
        let old_price = U128(self.latest_price);
        self.latest_price = price.into();
        ContractEvent::PriceUpdated {
            by: env::predecessor_account_id(),
            old_price,
            new_price: price,
        }
        .emit();
    }

    /// Returns the price of 1 cNEAR in yoctoNEAR (so 10^24 means 1 NEAR = 1 cNEAR), matching LiNEAR's `ft_price` interface. The value is set by the owner and reflects the rate at the most recent of:
    /// - the last issuance,
    /// - the last redemption, or
    /// - the monthly published redemption price derived from fund NAV.
    ///
    /// This is an indicative reference price, NOT a redemption quote. The only reliable redemption price is an actual quote (e.g. via NEAR Intents), and realized rates can differ substantially — especially during periods of heavy redemptions, when public floor prices may be withdrawn entirely. Do not use this as a price oracle: computing LTV from this value in a pool that lends NEAR against cNEAR risks user funds.
    pub fn get_latest_price(&self) -> U128 {
        self.latest_price.into()
    }

    pub fn owner_get(&self) -> AccountId {
        self.owner_id.clone()
    }

    /// Emits an `OwnerChanged` event.
    #[access_control_any(roles(Role::Owner))]
    pub fn owner_set(&mut self, new_owner: AccountId) {
        let old_owner = self.owner_id.clone();
        self.owner_id = new_owner.clone();
        // Transfer role to new owner
        self.acl_revoke_role(Role::Owner.into(), old_owner.clone());
        self.grant_owner_role(&new_owner);
        ContractEvent::OwnerChanged {
            old_owner,
            new_owner,
        }
        .emit();
    }

    fn grant_owner_role(&mut self, owner: &AccountId) {
        let mut acl = self.acl_get_or_init();
        acl.add_super_admin_unchecked(owner);
        acl.add_admin_unchecked(Role::Owner, owner);
        acl.grant_role_unchecked(Role::Owner, owner);
    }

    /// Owner-only transfer between arbitrary accounts. Not subject to the pause and freeze controls.
    /// Emits a `ForceFtTransfer` event in addition to the standard `FtTransfer` event, so indexers can distinguish forced movements.
    #[payable]
    #[access_control_any(roles(Role::Owner))]
    pub fn force_ft_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
    ) {
        assert_one_yocto();
        let amount_raw: u128 = amount.into();
        self.token
            .internal_transfer(&sender_id, &receiver_id, amount_raw, memo.clone());
        ContractEvent::ForceFtTransfer {
            by: env::predecessor_account_id(),
            sender_id,
            receiver_id,
            amount,
            memo,
        }
        .emit();
    }
}

#[near]
impl FungibleTokenCore for Contract {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        require!(!self.paused, "Token transfers paused");
        let sender_id = env::predecessor_account_id();
        require!(
            !self.frozen_accounts.contains(&sender_id),
            "Sender account is frozen"
        );
        require!(
            !self.frozen_accounts.contains(&receiver_id),
            "Receiver account is frozen"
        );
        self.token.ft_transfer(receiver_id, amount, memo)
    }

    #[payable]
    fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128> {
        require!(!self.paused, "Token transfers paused");
        let sender_id = env::predecessor_account_id();
        require!(
            !self.frozen_accounts.contains(&sender_id),
            "Sender account is frozen"
        );
        require!(
            !self.frozen_accounts.contains(&receiver_id),
            "Receiver account is frozen"
        );
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }

    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenResolver for Contract {
    #[private]
    fn ft_resolve_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
    ) -> U128 {
        let (used_amount, burned_amount) =
            self.token
                .internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        if burned_amount > 0 {
            log!("Account @{} burned {}", sender_id, burned_amount);
        }
        used_amount.into()
    }
}

#[near]
impl StorageManagement for Contract {
    #[payable]
    fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        #[allow(unused_variables)]
        if let Some((account_id, balance)) = self.token.internal_storage_unregister(force) {
            log!("Closed @{} with {}", account_id, balance);
            true
        } else {
            false
        }
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for Contract {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        self.metadata.get().unwrap()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use near_contract_standards::fungible_token::metadata::FT_METADATA_SPEC;
    use near_contract_standards::fungible_token::Balance;
    use near_sdk::test_utils::{accounts, VMContextBuilder};
    use near_sdk::{testing_env, Gas};

    use super::*;

    const TOTAL_SUPPLY: Balance = 1_000_000_000_000_000;
    const INITIAL_PRICE: u128 = 1612;

    fn current() -> AccountId {
        accounts(0)
    }

    fn owner() -> AccountId {
        accounts(1)
    }

    fn user1() -> AccountId {
        accounts(2)
    }

    fn user2() -> AccountId {
        accounts(3)
    }

    fn setup() -> (Contract, VMContextBuilder) {
        let mut context = VMContextBuilder::new();
        const DATA_IMAGE_SVG_NEAR_ICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 288 288'%3E%3Cg id='l' data-name='l'%3E%3Cpath d='M187.58,79.81l-30.1,44.69a3.2,3.2,0,0,0,4.75,4.2L191.86,103a1.2,1.2,0,0,1,2,.91v80.46a1.2,1.2,0,0,1-2.12.77L102.18,77.93A15.35,15.35,0,0,0,90.47,72.5H87.34A15.34,15.34,0,0,0,72,87.84V201.16A15.34,15.34,0,0,0,87.34,216.5h0a15.35,15.35,0,0,0,13.08-7.31l30.1-44.69a3.2,3.2,0,0,0-4.75-4.2L96.14,186a1.2,1.2,0,0,1-2-.91V104.61a1.2,1.2,0,0,1,2.12-.77l89.55,107.23a15.35,15.35,0,0,0,11.71,5.43h3.13A15.34,15.34,0,0,0,216,201.16V87.84A15.34,15.34,0,0,0,200.66,72.5h0A15.35,15.35,0,0,0,187.58,79.81Z'/%3E%3C/g%3E%3C/svg%3E";

        context.current_account_id(current());
        context.predecessor_account_id(current());
        testing_env!(context.build());

        let contract = Contract::new(
            owner(),
            TOTAL_SUPPLY.into(),
            FungibleTokenMetadata {
                spec: FT_METADATA_SPEC.to_string(),
                name: "Example NEAR fungible token".to_string(),
                symbol: "EXAMPLE".to_string(),
                icon: Some(DATA_IMAGE_SVG_NEAR_ICON.to_string()),
                reference: None,
                reference_hash: None,
                decimals: 24,
            },
            INITIAL_PRICE.into(),
        );

        context.storage_usage(env::storage_usage());

        testing_env!(context.build());

        (contract, context)
    }

    #[test]
    fn test_new() {
        let (contract, _) = setup();

        assert_eq!(contract.ft_total_supply().0, TOTAL_SUPPLY);
        assert_eq!(contract.ft_balance_of(owner()).0, TOTAL_SUPPLY);
    }

    #[test]
    fn test_metadata() {
        let (contract, _) = setup();

        assert_eq!(contract.ft_metadata().decimals, 24);
        assert!(contract.ft_metadata().icon.is_some());
        assert!(!contract.ft_metadata().spec.is_empty());
        assert!(!contract.ft_metadata().name.is_empty());
        assert!(!contract.ft_metadata().symbol.is_empty());
    }

    #[test]
    #[should_panic(expected = "The contract is not initialized")]
    fn test_default_panics() {
        Contract::default();
    }

    #[test]
    fn test_deposit() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        assert!(contract.storage_balance_of(user1()).is_none());

        contract.storage_deposit(None, None);

        let storage_balance = contract.storage_balance_of(user1()).unwrap();
        assert_eq!(storage_balance.total, contract.storage_balance_bounds().min);
        assert!(storage_balance.available.is_zero());
    }

    #[test]
    fn test_deposit_on_behalf_of_another_user() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        assert!(contract.storage_balance_of(user2()).is_none());

        // predecessor is user1, but deposit is for user2
        contract.storage_deposit(Some(user2()), None);

        let storage_balance = contract.storage_balance_of(user2()).unwrap();
        assert_eq!(storage_balance.total, contract.storage_balance_bounds().min);
        assert!(storage_balance.available.is_zero());

        // ensure that user1's storage wasn't affected
        assert!(contract.storage_balance_of(user1()).is_none());
    }

    #[should_panic]
    #[test]
    fn test_deposit_panics_on_less_amount() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(100))
            .build());

        assert!(contract.storage_balance_of(user1()).is_none());

        // this panics
        contract.storage_deposit(None, None);
    }

    #[test]
    fn test_deposit_account_twice() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // this registers the predecessor
        contract.storage_deposit(None, None);

        let storage_balance = contract.storage_balance_of(user1()).unwrap();
        assert_eq!(storage_balance.total, contract.storage_balance_bounds().min);

        // this doesn't panic, and just refunds the deposit as the account is registered already
        contract.storage_deposit(None, None);

        // this indicates that total balance hasn't changed
        let storage_balance = contract.storage_balance_of(user1()).unwrap();
        assert_eq!(storage_balance.total, contract.storage_balance_bounds().min);
    }

    #[test]
    fn test_unregister() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        contract.storage_deposit(None, None);

        assert!(contract.storage_balance_of(user1()).is_some());

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        assert!(contract.storage_unregister(None));

        assert!(contract.storage_balance_of(user1()).is_none());
    }

    #[should_panic]
    #[test]
    fn test_unregister_panics_on_zero_deposit() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        contract.storage_deposit(None, None);

        assert!(contract.storage_balance_of(user1()).is_some());

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(0))
            .build());

        contract.storage_unregister(None);
    }

    #[test]
    fn test_unregister_of_non_registered_account() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        // "false" indicates that the account wasn't registered
        assert!(!contract.storage_unregister(None));
    }

    #[should_panic]
    #[test]
    fn test_unregister_panics_on_non_zero_balance() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        contract.storage_deposit(None, None);

        assert!(contract.storage_balance_of(user1()).is_some());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;

        contract.ft_transfer(user1(), transfer_amount.into(), None);

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        contract.storage_unregister(None);
    }

    #[test]
    fn test_unregister_with_force() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        contract.storage_deposit(None, None);

        assert!(contract.storage_balance_of(user1()).is_some());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;

        contract.ft_transfer(user1(), transfer_amount.into(), None);

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        // force to unregister no matter what
        // this reduces total supply because user's tokens are burnt
        assert!(contract.storage_unregister(Some(true)));

        assert!(contract.storage_balance_of(user1()).is_none());
        assert_eq!(contract.ft_balance_of(user1()).0, 0);
        assert_eq!(contract.ft_total_supply().0, TOTAL_SUPPLY - transfer_amount);
    }

    #[test]
    fn test_withdraw() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        // Basic Fungible Token implementation never transfers Near to caller
        // See: https://github.com/near/near-sdk-rs/blob/5a4c595125364ffe8d7866aa0418a3c92b1c3a6a/near-contract-standards/src/fungible_token/storage_impl.rs#L82
        let storage_balance = contract.storage_withdraw(None);
        assert_eq!(storage_balance.total, contract.storage_balance_bounds().min);
        assert!(storage_balance.available.is_zero());

        // Basic Fungible Token implementation never transfers Near to caller
        // See: https://github.com/near/near-sdk-rs/blob/5a4c595125364ffe8d7866aa0418a3c92b1c3a6a/near-contract-standards/src/fungible_token/storage_impl.rs#L82
        let storage_balance = contract.storage_withdraw(None);
        assert_eq!(storage_balance.total, contract.storage_balance_bounds().min);
        assert!(storage_balance.available.is_zero());
    }

    #[should_panic]
    #[test]
    fn test_withdraw_panics_on_non_registered_account() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        contract.storage_withdraw(None);
    }

    #[should_panic]
    #[test]
    fn test_withdraw_panics_on_zero_deposit() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(0))
            .build());

        contract.storage_withdraw(None);
    }

    #[should_panic]
    #[test]
    fn test_withdraw_panics_on_amount_greater_than_zero() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        // Basic Fungible Token implementation sets storage_balance_bounds.min == storage_balance_bounds.max
        // which means available balance will always be 0
        // See: https://github.com/near/near-sdk-rs/blob/5a4c595125364ffe8d7866aa0418a3c92b1c3a6a/near-contract-standards/src/fungible_token/storage_impl.rs#L82
        contract.storage_withdraw(Some(NearToken::from_yoctonear(1)));
    }

    #[test]
    fn test_transfer() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;

        contract.ft_transfer(user1(), transfer_amount.into(), None);

        assert_eq!(
            contract.ft_balance_of(owner()).0,
            (TOTAL_SUPPLY - transfer_amount)
        );
        assert_eq!(contract.ft_balance_of(user1()).0, transfer_amount);
    }

    #[should_panic]
    #[test]
    fn test_transfer_panics_on_self_receiver() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;

        contract.ft_transfer(owner(), transfer_amount.into(), None);
    }

    #[should_panic]
    #[test]
    fn test_transfer_panics_on_zero_amount() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        contract.ft_transfer(user1(), 0.into(), None);
    }

    #[should_panic]
    #[test]
    fn test_transfer_panics_on_zero_deposit() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(0))
            .build());

        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.ft_transfer(user1(), transfer_amount.into(), None);
    }

    #[should_panic]
    #[test]
    fn test_transfer_panics_on_non_registered_sender() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.ft_transfer(user1(), transfer_amount.into(), None);
    }

    #[should_panic]
    #[test]
    fn test_transfer_panics_on_non_registered_receiver() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.ft_transfer(user1(), transfer_amount.into(), None);
    }

    #[should_panic]
    #[test]
    fn test_transfer_panics_on_amount_greater_than_balance() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        let transfer_amount = TOTAL_SUPPLY + 10;
        contract.ft_transfer(user1(), transfer_amount.into(), None);
    }

    #[test]
    fn test_transfer_call() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;

        let _ = contract.ft_transfer_call(user1(), transfer_amount.into(), None, "".to_string());

        assert_eq!(
            contract.ft_balance_of(owner()).0,
            (TOTAL_SUPPLY - transfer_amount)
        );
        assert_eq!(contract.ft_balance_of(user1()).0, transfer_amount);
    }

    #[should_panic]
    #[test]
    fn test_transfer_call_panics_on_self_receiver() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;

        let _ = contract.ft_transfer_call(owner(), transfer_amount.into(), None, "".to_string());
    }

    #[should_panic]
    #[test]
    fn test_transfer_call_panics_on_zero_amount() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        let _ = contract.ft_transfer_call(user1(), 0.into(), None, "".to_string());
    }

    #[should_panic]
    #[test]
    fn test_transfer_call_panics_on_zero_deposit() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(0))
            .build());

        let transfer_amount = TOTAL_SUPPLY / 10;
        let _ = contract.ft_transfer_call(user1(), transfer_amount.into(), None, "".to_string());
    }

    #[should_panic]
    #[test]
    fn test_transfer_call_panics_on_non_registered_sender() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        let transfer_amount = TOTAL_SUPPLY / 10;
        let _ = contract.ft_transfer_call(user1(), transfer_amount.into(), None, "".to_string());
    }

    #[should_panic]
    #[test]
    fn test_transfer_call_panics_on_non_registered_receiver() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        let transfer_amount = TOTAL_SUPPLY / 10;
        let _ = contract.ft_transfer_call(user1(), transfer_amount.into(), None, "".to_string());
    }

    #[should_panic]
    #[test]
    fn test_transfer_call_panics_on_amount_greater_than_balance() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());

        let transfer_amount = TOTAL_SUPPLY + 10;
        let _ = contract.ft_transfer_call(user1(), transfer_amount.into(), None, "".to_string());
    }
    #[should_panic]
    #[test]
    fn test_transfer_call_panics_on_unsufficient_gas() {
        let (mut contract, mut context) = setup();

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());

        // Paying for account registration of user1, aka storage deposit
        contract.storage_deposit(None, None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .prepaid_gas(Gas::from_tgas(10))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;

        let _ = contract.ft_transfer_call(user1(), transfer_amount.into(), None, "".to_string());
    }

    fn register_user(
        contract: &mut Contract,
        context: &mut VMContextBuilder,
        account_id: AccountId,
    ) {
        testing_env!(context
            .predecessor_account_id(account_id.clone())
            .attached_deposit(contract.storage_balance_bounds().min)
            .build());
        contract.storage_deposit(None, None);
    }

    #[test]
    fn test_force_transfer() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());
        register_user(&mut contract, &mut context, user2());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.force_ft_transfer(owner(), user1(), transfer_amount.into(), None);

        assert_eq!(
            contract.ft_balance_of(owner()).0,
            TOTAL_SUPPLY - transfer_amount
        );
        assert_eq!(contract.ft_balance_of(user1()).0, transfer_amount);
    }

    #[test]
    fn test_force_transfer_between_non_owner_accounts() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());
        register_user(&mut contract, &mut context, user2());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.force_ft_transfer(owner(), user1(), transfer_amount.into(), None);

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.force_ft_transfer(user1(), user2(), transfer_amount.into(), None);

        assert_eq!(contract.ft_balance_of(user1()).0, 0);
        assert_eq!(contract.ft_balance_of(user2()).0, transfer_amount);
    }

    #[should_panic]
    #[test]
    fn test_force_transfer_panics_on_zero_deposit() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(0))
            .build());
        contract.force_ft_transfer(owner(), user1(), (TOTAL_SUPPLY / 10).into(), None);
    }

    #[test]
    fn test_freeze_account() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.freeze_account(user1());

        assert!(contract.is_frozen(user1()));
        assert!(!contract.is_frozen(user2()));
    }

    #[test]
    fn test_unfreeze_account() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.freeze_account(user1());
        assert!(contract.is_frozen(user1()));

        contract.unfreeze_account(user1());
        assert!(!contract.is_frozen(user1()));
    }

    #[should_panic(expected = "Sender account is frozen")]
    #[test]
    fn test_transfer_panics_when_sender_frozen() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());
        register_user(&mut contract, &mut context, user2());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.force_ft_transfer(owner(), user1(), (TOTAL_SUPPLY / 10).into(), None);

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.freeze_account(user1());

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.ft_transfer(user2(), (TOTAL_SUPPLY / 10).into(), None);
    }

    #[should_panic(expected = "Receiver account is frozen")]
    #[test]
    fn test_transfer_panics_when_receiver_frozen() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.freeze_account(user1());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.ft_transfer(user1(), (TOTAL_SUPPLY / 10).into(), None);
    }

    #[test]
    fn test_transfer_succeeds_after_unfreeze() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.freeze_account(user1());
        contract.unfreeze_account(user1());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.ft_transfer(user1(), transfer_amount.into(), None);

        assert_eq!(contract.ft_balance_of(user1()).0, transfer_amount);
    }

    #[test]
    fn test_pause_by_owner() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause();

        assert!(contract.is_paused());
    }

    #[test]
    fn test_unpause_by_owner() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause();
        assert!(contract.is_paused());

        contract.unpause();
        assert!(!contract.is_paused());
    }

    #[should_panic(expected = "Token transfers paused")]
    #[test]
    fn test_transfer_panics_when_paused() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.ft_transfer(user1(), transfer_amount.into(), None);

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause();

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.ft_transfer(user1(), transfer_amount.into(), None);
    }

    #[should_panic(expected = "Token transfers paused")]
    #[test]
    fn test_transfer_call_panics_when_paused() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause();

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let _ =
            contract.ft_transfer_call(user1(), (TOTAL_SUPPLY / 10).into(), None, "".to_string());
    }

    #[test]
    fn test_is_paused_returns_false_initially() {
        let (contract, _) = setup();
        assert!(!contract.is_paused());
    }

    #[test]
    fn test_is_frozen_returns_false_initially() {
        let (contract, _) = setup();
        assert!(!contract.is_frozen(user1()));
    }

    #[test]
    fn test_pause_idempotent() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause();
        assert!(contract.is_paused());

        contract.pause();
        assert!(contract.is_paused());
    }

    #[should_panic(expected = "Sender account is frozen")]
    #[test]
    fn test_transfer_panics_on_both_frozen_sender_first() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());
        register_user(&mut contract, &mut context, user2());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.force_ft_transfer(owner(), user1(), transfer_amount.into(), None);

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.freeze_account(user1());
        contract.freeze_account(user2());

        testing_env!(context
            .predecessor_account_id(user1())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.ft_transfer(user2(), transfer_amount.into(), None);
    }

    #[should_panic(expected = "Token transfers paused")]
    #[test]
    fn test_transfer_paused_check_before_frozen_check() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause();
        contract.freeze_account(user1());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.ft_transfer(user1(), transfer_amount.into(), None);
    }

    #[test]
    fn test_transfer_after_pause_unpause_cycle() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause();
        contract.unpause();

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let transfer_amount = TOTAL_SUPPLY / 10;
        contract.ft_transfer(user1(), transfer_amount.into(), None);

        assert_eq!(contract.ft_balance_of(user1()).0, transfer_amount);
    }

    #[test]
    fn test_latest_price_set_initially() {
        let (contract, _) = setup();
        assert_eq!(contract.get_latest_price().0, INITIAL_PRICE);
    }

    #[test]
    fn test_set_latest_price_by_owner() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.set_latest_price(1_000_000.into());

        assert_eq!(contract.get_latest_price().0, 1_000_000);
    }

    #[should_panic]
    #[test]
    fn test_set_latest_price_panics_on_non_owner() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(user1()).build());
        contract.set_latest_price(1_000_000.into());
    }

    #[should_panic(expected = "Receiver account is frozen")]
    #[test]
    fn test_transfer_call_panics_on_frozen_receiver() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.freeze_account(user1());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        let _ =
            contract.ft_transfer_call(user1(), (TOTAL_SUPPLY / 10).into(), None, "".to_string());
    }

    fn cnear_events() -> Vec<String> {
        near_sdk::test_utils::get_logs()
            .into_iter()
            .filter(|log| log.starts_with("EVENT_JSON:") && log.contains("\"standard\":\"cnear\""))
            .collect()
    }

    #[test]
    fn test_pause_emits_event_on_every_call() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause();
        let events = cnear_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("\"event\":\"paused\""));
        assert!(events[0].contains(&format!("\"by\":\"{}\"", owner())));

        // Privileged actions are audited unconditionally: re-pausing an
        // already-paused contract still emits an event.
        contract.pause();
        assert_eq!(cnear_events().len(), 2);
    }

    #[test]
    fn test_pause_contract_emits_exactly_one_event() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.pause_contract();
        let events = cnear_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("\"event\":\"paused\""));
    }

    #[test]
    fn test_unpause_emits_event_on_every_call() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        // Unpausing an unpaused contract is a state no-op but is still audited.
        contract.unpause();
        let events = cnear_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("\"event\":\"unpaused\""));

        contract.pause();
        contract.unpause();
        let events = cnear_events();
        assert_eq!(events.len(), 3);
        assert!(events[1].contains("\"event\":\"paused\""));
        assert!(events[2].contains("\"event\":\"unpaused\""));
    }

    #[test]
    fn test_freeze_unfreeze_emit_events_on_every_call() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.freeze_account(user1());
        contract.freeze_account(user1()); // state no-op, still audited
        contract.unfreeze_account(user1());
        contract.unfreeze_account(user1()); // state no-op, still audited

        let events = cnear_events();
        assert_eq!(events.len(), 4);
        assert!(events[0].contains("\"event\":\"account_frozen\""));
        assert!(events[0].contains(&format!("\"account_id\":\"{}\"", user1())));
        assert!(events[1].contains("\"event\":\"account_frozen\""));
        assert!(events[2].contains("\"event\":\"account_unfrozen\""));
        assert!(events[3].contains("\"event\":\"account_unfrozen\""));
    }

    #[test]
    fn test_set_latest_price_emits_event_with_old_and_new_values() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.set_latest_price(1_000_000.into());

        let events = cnear_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("\"event\":\"price_updated\""));
        assert!(events[0].contains(&format!("\"old_price\":\"{INITIAL_PRICE}\"")));
        assert!(events[0].contains("\"new_price\":\"1000000\""));
    }

    #[test]
    fn test_owner_set_emits_owner_changed_event() {
        let (mut contract, mut context) = setup();

        testing_env!(context.predecessor_account_id(owner()).build());
        contract.owner_set(user1());

        let events = cnear_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("\"event\":\"owner_changed\""));
        assert!(events[0].contains(&format!("\"old_owner\":\"{}\"", owner())));
        assert!(events[0].contains(&format!("\"new_owner\":\"{}\"", user1())));
    }

    #[test]
    fn test_force_transfer_emits_dedicated_event() {
        let (mut contract, mut context) = setup();
        register_user(&mut contract, &mut context, user1());

        testing_env!(context
            .predecessor_account_id(owner())
            .attached_deposit(NearToken::from_yoctonear(1))
            .build());
        contract.force_ft_transfer(owner(), user1(), (TOTAL_SUPPLY / 10).into(), None);

        let events = cnear_events();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("\"event\":\"force_ft_transfer\""));
        assert!(events[0].contains(&format!("\"sender_id\":\"{}\"", owner())));
        assert!(events[0].contains(&format!("\"receiver_id\":\"{}\"", user1())));
        // The standard NEP-141 FtTransfer event must also still be emitted.
        assert!(near_sdk::test_utils::get_logs()
            .iter()
            .any(|log| log.contains("\"standard\":\"nep141\"") && log.contains("ft_transfer")));
    }
}
