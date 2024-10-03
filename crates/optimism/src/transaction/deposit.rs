use revm::{
    primitives::{Address, Bytes, TxKind, B256, U256},
    transaction::CommonTxFields,
};

pub trait DepositTransaction: CommonTxFields {
    fn source_hash(&self) -> B256;

    fn to(&self) -> TxKind;

    fn mint(&self) -> Option<u128>;

    fn is_system_transaction(&self) -> bool;
}

pub struct TxDeposit {
    /// Hash that uniquely identifies the source of the deposit.
    pub source_hash: B256,
    /// The address of the sender account.
    pub from: Address,
    /// The address of the recipient account, or the null (zero-length) address if the deposited
    /// transaction is a contract creation.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "TxKind::is_create")
    )]
    pub to: TxKind,
    /// The ETH value to mint on L2.
    #[cfg_attr(feature = "serde", serde(default, with = "alloy_serde::quantity::opt"))]
    pub mint: Option<u128>,
    ///  The ETH value to send to the recipient account.
    pub value: U256,
    /// The gas limit for the L2 transaction.
    #[cfg_attr(
        feature = "serde",
        serde(with = "alloy_serde::quantity", rename = "gas")
    )]
    pub gas_limit: u64,
    /// Field indicating if this transaction is exempt from the L2 gas limit.
    #[cfg_attr(
        feature = "serde",
        serde(with = "alloy_serde::quantity", rename = "isSystemTx")
    )]
    pub is_system_transaction: bool,
    /// Input has two uses depending if transaction is Create or Call (if `to` field is None or
    /// Some).
    pub input: Bytes,
}

impl CommonTxFields for TxDeposit {
    fn caller(&self) -> Address {
        self.from
    }

    fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    fn value(&self) -> U256 {
        self.value
    }

    fn input(&self) -> &Bytes {
        &self.input
    }

    fn nonce(&self) -> u64 {
        panic!("There is no nonce in a deposit transaction");
    }
}

impl DepositTransaction for TxDeposit {
    fn source_hash(&self) -> B256 {
        self.source_hash
    }

    fn to(&self) -> TxKind {
        self.to
    }

    fn mint(&self) -> Option<u128> {
        self.mint
    }

    fn is_system_transaction(&self) -> bool {
        self.is_system_transaction
    }
}