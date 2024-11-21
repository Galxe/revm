use crate::{
    interpreter::{Gas, SuccessOrHalt},
    primitives::{
        db::Database, EVMError, Env, ExecutionResult, ResultAndState, Spec, SpecId, SpecId::LONDON,
        U256,
    },
    Context, FrameResult,
};

/// Mainnet end handle does not change the output.
#[inline]
pub fn end<EXT, DB: Database>(
    _context: &mut Context<EXT, DB>,
    evm_output: Result<ResultAndState, EVMError<DB::Error>>,
) -> Result<ResultAndState, EVMError<DB::Error>> {
    evm_output
}

/// Clear handle clears error and journal state.
#[inline]
pub fn clear<EXT, DB: Database>(context: &mut Context<EXT, DB>) {
    // clear error and journaled state.
    let _ = context.evm.take_error();
    context.evm.inner.journaled_state.clear();
}

/// Reward beneficiary with gas fee.
#[inline]
fn reward_beneficiary<EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    rewards: u128,
) -> Result<(), EVMError<DB::Error>> {
    let beneficiary = context.evm.env.block.coinbase;

    let coinbase_account = context
        .evm
        .inner
        .journaled_state
        .load_account(beneficiary, &mut context.evm.inner.db)?;

    coinbase_account.data.mark_touch();
    coinbase_account.data.info.balance = coinbase_account
        .data
        .info
        .balance
        .saturating_add(U256::from(rewards));

    Ok(())
}

#[inline]
fn reward<SPEC: Spec>(env: &Env, gas: &Gas) -> u128 {
    let effective_gas_price = env.effective_gas_price();

    // EIP-1559 discard basefee for coinbase transfer. Basefee amount of gas is discarded.
    let coinbase_gas_price = if SPEC::enabled(LONDON) {
        effective_gas_price.saturating_sub(env.block.basefee)
    } else {
        effective_gas_price
    };

    coinbase_gas_price.to::<u128>() * (gas.spent() as u128 - gas.refunded() as u128)
}

pub fn refund<SPEC: Spec, EXT, DB: Database>(
    _context: &mut Context<EXT, DB>,
    gas: &mut Gas,
    eip7702_refund: i64,
) {
    gas.record_refund(eip7702_refund);

    // Calculate gas refund for transaction.
    // If spec is set to london, it will decrease the maximum refund amount to 5th part of
    // gas spend. (Before london it was 2th part of gas spend)
    gas.set_final_refund(SPEC::SPEC_ID.is_enabled_in(SpecId::LONDON));
}

#[inline]
pub fn reimburse_caller<SPEC: Spec, EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    gas: &Gas,
) -> Result<(), EVMError<DB::Error>> {
    let caller = context.evm.env.tx.caller;
    let effective_gas_price = context.evm.env.effective_gas_price();

    // return balance of not spend gas.
    let caller_account = context
        .evm
        .inner
        .journaled_state
        .load_account(caller, &mut context.evm.inner.db)?;

    caller_account.data.info.balance =
        caller_account.data.info.balance.saturating_add(
            effective_gas_price * U256::from(gas.remaining() + gas.refunded() as u64),
        );

    Ok(())
}

/// Main return handle, returns the output of the transaction.
#[inline]
pub fn output<SPEC: Spec, EXT, DB: Database>(
    context: &mut Context<EXT, DB>,
    result: FrameResult,
    lazy_reward: bool,
) -> Result<ResultAndState, EVMError<DB::Error>> {
    let rewards = reward::<SPEC>(context.evm.env.as_ref(), result.gas());
    if !lazy_reward {
        reward_beneficiary(context, rewards)?;
    }

    context.evm.take_error()?;
    // used gas with refund calculated.
    let gas_refunded = result.gas().refunded() as u64;
    let final_gas_used = result.gas().spent() - gas_refunded;
    let output = result.output();
    let instruction_result = result.into_interpreter_result();

    // reset journal and return present state.
    let (state, logs) = context.evm.journaled_state.finalize();

    let result = match instruction_result.result.into() {
        SuccessOrHalt::Success(reason) => ExecutionResult::Success {
            reason,
            gas_used: final_gas_used,
            gas_refunded,
            logs,
            output,
        },
        SuccessOrHalt::Revert => ExecutionResult::Revert {
            gas_used: final_gas_used,
            output: output.into_data(),
        },
        SuccessOrHalt::Halt(reason) => ExecutionResult::Halt {
            reason,
            gas_used: final_gas_used,
        },
        // Only two internal return flags.
        flag @ (SuccessOrHalt::FatalExternalError | SuccessOrHalt::Internal(_)) => {
            panic!(
                "Encountered unexpected internal return flag: {:?} with instruction result: {:?}",
                flag, instruction_result
            )
        }
    };

    Ok(ResultAndState {
        result,
        state,
        rewards,
    })
}
