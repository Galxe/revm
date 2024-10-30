use crate::{
    gas,
    interpreter::NewInterpreter,
    interpreter_wiring::{InterpreterWire, LoopControl, RuntimeFlag, StackTrait},
    Host,
};
use primitives::U256;
use transaction::Eip4844Tx;
use wiring::{Block, Transaction, TransactionType};

pub fn gasprice<WIRE: InterpreterWire, H: Host + ?Sized>(
    interpreter: &mut NewInterpreter<WIRE>,
    host: &mut H,
) {
    gas!(interpreter, gas::BASE);
    let env = host.env();
    let basefee = *env.block.basefee();
    push!(interpreter, env.tx.effective_gas_price(basefee));
    push!(interpreter, U256::ZERO)
}

pub fn origin<WIRE: InterpreterWire, H: Host + ?Sized>(
    interpreter: &mut NewInterpreter<WIRE>,
    host: &mut H,
) {
    gas!(interpreter, gas::BASE);
    push!(
        interpreter,
        host.env().tx.common_fields().caller().into_word().into()
    );
}

// EIP-4844: Shard Blob Transactions
pub fn blob_hash<WIRE: InterpreterWire, H: Host + ?Sized>(
    interpreter: &mut NewInterpreter<WIRE>,
    host: &mut H,
) {
    check!(interpreter, CANCUN);
    gas!(interpreter, gas::VERYLOW);
    popn_top!([], index, interpreter);
    let i = as_usize_saturated!(index);
    let tx = &host.env().tx;
    *index = if tx.tx_type().into() == TransactionType::Eip4844 {
        tx.eip4844()
            .blob_versioned_hashes()
            .get(i)
            .cloned()
            .map(|b| U256::from_be_bytes(*b))
            .unwrap_or(U256::ZERO)
    } else {
        U256::ZERO
    };
}
