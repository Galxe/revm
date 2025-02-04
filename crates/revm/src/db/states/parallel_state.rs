use super::{
    bundle_state::BundleRetention, plain_account::PlainStorage, BundleState, CacheAccount, ParallelCacheState, TransitionAccount, TransitionState
};
use core::hash::{BuildHasherDefault, Hasher};
use dashmap::{mapref::one::RefMut, DashMap, Entry};
use revm_interpreter::primitives::{
    db::{Database, DatabaseCommit, DatabaseRef},
    hash_map, Account, AccountInfo, Address, Bytecode, HashMap, B256, U256,
};
use std::vec::Vec;

#[derive(Debug, Default)]
pub struct IdentityHasher(u64);
impl Hasher for IdentityHasher {
    fn write_u64(&mut self, id: u64) {
        self.0 = id;
    }
    fn write_usize(&mut self, id: usize) {
        self.0 = id as u64;
    }
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
}
pub type BuildIdentityHasher = BuildHasherDefault<IdentityHasher>;

/// State of blockchain.
///
/// State clear flag is set inside CacheState and by default it is enabled.
/// If you want to disable it use `set_state_clear_flag` function.
#[derive(Debug)]
pub struct ParallelState<DB> {
    /// Cached state contains both changed from evm execution and cached/loaded account/storages
    /// from database. This allows us to have only one layer of cache where we can fetch data.
    /// Additionally we can introduce some preloading of data from database.
    pub cache: ParallelCacheState,
    /// Optional database that we use to fetch data from. If database is not present, we will
    /// return not existing account and storage.
    ///
    /// Note: It is marked as Send so database can be shared between threads.
    pub database: DB,
    /// Block state, it aggregates transactions transitions into one state.
    ///
    /// Build reverts and state that gets applied to the state.
    pub transition_state: Option<TransitionState>,
    /// After block is finishes we merge those changes inside bundle.
    /// Bundle is used to update database and create changesets.
    /// Bundle state can be set on initialization if we want to use preloaded bundle.
    pub bundle_state: BundleState,
    /// Addition layer that is going to be used to fetched values before fetching values
    /// from database.
    ///
    /// Bundle is the main output of the state execution and this allows setting previous bundle
    /// and using its values for execution.
    pub use_preloaded_bundle: bool,
    /// If EVM asks for block hash we will first check if they are found here.
    /// and then ask the database.
    ///
    /// This map can be used to give different values for block hashes if in case
    /// The fork block is different or some blocks are not saved inside database.
    pub block_hashes: DashMap<u64, B256, BuildIdentityHasher>,
}

impl<DB: DatabaseRef> ParallelState<DB> {
    pub fn new(database: DB, with_bundle_update: bool) -> Self {
        Self {
            cache: ParallelCacheState::default(),
            database,
            transition_state: with_bundle_update.then(TransitionState::default),
            bundle_state: BundleState::default(),
            use_preloaded_bundle: false,
            block_hashes: DashMap::default(),
        }
    }

    /// Returns the size hint for the inner bundle state.
    /// See [BundleState::size_hint] for more info.
    pub fn bundle_size_hint(&self) -> usize {
        self.bundle_state.size_hint()
    }

    /// Iterate over received balances and increment all account balances.
    /// If account is not found inside cache state it will be loaded from database.
    ///
    /// Update will create transitions for all accounts that are updated.
    ///
    /// Like [CacheAccount::increment_balance], this assumes that incremented balances are not
    /// zero, and will not overflow once incremented. If using this to implement withdrawals, zero
    /// balances must be filtered out before calling this function.
    pub fn increment_balances(
        &mut self,
        balances: impl IntoIterator<Item = (Address, u128)>,
    ) -> Result<(), DB::Error> {
        // make transition and update cache state
        let mut transitions = Vec::new();
        for (address, balance) in balances {
            if balance == 0 {
                continue;
            }
            let mut original_account = self.load_cache_account(address)?;
            transitions.push((
                address,
                original_account
                    .increment_balance(balance)
                    .expect("Balance is not zero"),
            ))
        }
        // append transition
        if let Some(s) = self.transition_state.as_mut() {
            s.add_transitions(transitions)
        }
        Ok(())
    }

    /// Drain balances from given account and return those values.
    ///
    /// It is used for DAO hardfork state change to move values from given accounts.
    pub fn drain_balances(
        &mut self,
        addresses: impl IntoIterator<Item = Address>,
    ) -> Result<Vec<u128>, DB::Error> {
        // make transition and update cache state
        let mut transitions = Vec::new();
        let mut balances = Vec::new();
        for address in addresses {
            let mut original_account = self.load_cache_account(address)?;
            let (balance, transition) = original_account.drain_balance();
            balances.push(balance);
            transitions.push((address, transition))
        }
        // append transition
        if let Some(s) = self.transition_state.as_mut() {
            s.add_transitions(transitions)
        }
        Ok(balances)
    }

    /// State clear EIP-161 is enabled in Spurious Dragon hardfork.
    pub fn set_state_clear_flag(&mut self, has_state_clear: bool) {
        self.cache.set_state_clear_flag(has_state_clear);
    }

    pub fn insert_not_existing(&mut self, address: Address) {
        self.cache.insert_not_existing(address)
    }

    pub fn insert_account(&mut self, address: Address, info: AccountInfo) {
        self.cache.insert_account(address, info)
    }

    pub fn insert_account_with_storage(
        &mut self,
        address: Address,
        info: AccountInfo,
        storage: PlainStorage,
    ) {
        self.cache
            .insert_account_with_storage(address, info, storage)
    }

    /// Apply evm transitions to transition state.
    pub fn apply_transition(&mut self, transitions: Vec<(Address, TransitionAccount)>) {
        // add transition to transition state.
        if let Some(s) = self.transition_state.as_mut() {
            s.add_transitions(transitions)
        }
    }

    /// Take all transitions and merge them inside bundle state.
    /// This action will create final post state and all reverts so that
    /// we at any time revert state of bundle to the state before transition
    /// is applied.
    pub fn merge_transitions(&mut self, retention: BundleRetention) {
        if let Some(transition_state) = self.transition_state.as_mut().map(TransitionState::take) {
            self.bundle_state
                .apply_transitions_and_create_reverts(transition_state, retention);
        }
    }

    /// Get a mutable reference to the [`CacheAccount`] for the given address.
    /// If the account is not found in the cache, it will be loaded from the
    /// database and inserted into the cache.
    pub fn load_cache_account(
        &self,
        address: Address,
    ) -> Result<RefMut<'_, Address, CacheAccount>, DB::Error> {
        match self.cache.accounts.entry(address) {
            Entry::Vacant(entry) => {
                if self.use_preloaded_bundle {
                    // load account from bundle state
                    if let Some(account) =
                        self.bundle_state.account(&address).cloned().map(Into::into)
                    {
                        return Ok(entry.insert(account));
                    }
                }
                // if not found in bundle, load it from database
                let info = self.database.basic_ref(address)?;
                let account = match info {
                    None => CacheAccount::new_loaded_not_existing(),
                    Some(acc) if acc.is_empty() => {
                        CacheAccount::new_loaded_empty_eip161(HashMap::default())
                    }
                    Some(acc) => CacheAccount::new_loaded(acc, HashMap::default()),
                };
                Ok(entry.insert(account))
            }
            Entry::Occupied(entry) => Ok(entry.into_ref()),
        }
    }

    // TODO make cache aware of transitions dropping by having global transition counter.
    /// Takes the [`BundleState`] changeset from the [`State`], replacing it
    /// with an empty one.
    ///
    /// This will not apply any pending [`TransitionState`]. It is recommended
    /// to call [`State::merge_transitions`] before taking the bundle.
    ///
    /// If the `State` has been built with the
    /// [`StateBuilder::with_bundle_prestate`] option, the pre-state will be
    /// taken along with any changes made by [`State::merge_transitions`].
    pub fn take_bundle(&mut self) -> BundleState {
        core::mem::take(&mut self.bundle_state)
    }

    // Database stuff
    //
    fn db_basic(&self, address: Address) -> Result<Option<AccountInfo>, DB::Error> {
        self.load_cache_account(address).map(|a| a.account_info())
    }

    fn db_code_by_hash(&self, code_hash: B256) -> Result<Bytecode, DB::Error> {
        let res = match self.cache.contracts.entry(code_hash) {
            Entry::Occupied(entry) => Ok(entry.get().clone()),
            Entry::Vacant(entry) => {
                if self.use_preloaded_bundle {
                    if let Some(code) = self.bundle_state.contracts.get(&code_hash) {
                        entry.insert(code.clone());
                        return Ok(code.clone());
                    }
                }
                // if not found in bundle ask database
                let code = self.database.code_by_hash_ref(code_hash)?;
                entry.insert(code.clone());
                Ok(code)
            }
        };
        res
    }

    fn db_storage(&self, address: Address, index: U256) -> Result<U256, DB::Error> {
        // Account is guaranteed to be loaded.
        // Note that storage from bundle is already loaded with account.
        if let Some(mut account) = self.cache.accounts.get_mut(&address) {
            // account will always be some, but if it is not, U256::ZERO will be returned.
            let is_storage_known = account.status.is_storage_known();
            Ok(account
                .account
                .as_mut()
                .map(|account| match account.storage.entry(index) {
                    hash_map::Entry::Occupied(entry) => Ok(*entry.get()),
                    hash_map::Entry::Vacant(entry) => {
                        // if account was destroyed or account is newly built
                        // we return zero and don't ask database.
                        let value = if is_storage_known {
                            U256::ZERO
                        } else {
                            self.database.storage_ref(address, index)?
                        };
                        entry.insert(value);
                        Ok(value)
                    }
                })
                .transpose()?
                .unwrap_or_default())
        } else {
            unreachable!("For accessing any storage account is guaranteed to be loaded beforehand")
        }
    }

    fn db_block_hash(&self, number: u64) -> Result<B256, DB::Error> {
        match self.block_hashes.entry(number) {
            Entry::Occupied(entry) => Ok(*entry.get()),
            Entry::Vacant(entry) => {
                let ret = *entry.insert(self.database.block_hash_ref(number)?);
                Ok(ret)
            }
        }
    }
}

impl<DB: DatabaseRef> Database for ParallelState<DB> {
    type Error = DB::Error;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.db_basic(address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.db_code_by_hash(code_hash)
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.db_storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.db_block_hash(number)
    }
}

impl<DB: DatabaseRef> DatabaseRef for ParallelState<DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.db_basic(address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.db_code_by_hash(code_hash)
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.db_storage(address, index)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.db_block_hash(number)
    }
}

impl<DB: DatabaseRef> DatabaseCommit for ParallelState<DB> {
    fn commit(&mut self, evm_state: HashMap<Address, Account>) {
        let transitions = self.cache.apply_evm_state(evm_state);
        self.apply_transition(transitions);
    }
}
