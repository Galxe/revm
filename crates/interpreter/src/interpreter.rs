pub mod ext_bytecode;
mod input;
mod loop_control;
mod return_data;
mod runtime_flags;
#[cfg(feature = "serde")]
pub mod serde;
mod shared_memory;
mod stack;
mod subroutine_stack;

use crate::{interpreter_wiring::*, Gas, Host, Instruction, InstructionResult, InterpreterAction};
use bytecode::Bytecode;

use core::cell::RefCell;
pub use ext_bytecode::ExtBytecode;
pub use input::InputsImpl;
use loop_control::LoopControl as LoopControlImpl;
use primitives::Bytes;
use return_data::ReturnDataImpl;
pub use runtime_flags::RuntimeFlags;
pub use shared_memory::{num_words, MemoryGetter, SharedMemory, EMPTY_SHARED_MEMORY};
use specification::hardfork::SpecId;
pub use stack::{Stack, STACK_LIMIT};
use std::rc::Rc;
use subroutine_stack::SubRoutineImpl;

#[derive(Debug, Clone)]
pub struct NewInterpreter<WIRE: InterpreterWire> {
    pub bytecode: WIRE::Bytecode,
    pub stack: WIRE::Stack,
    pub return_data: WIRE::ReturnData,
    pub memory: WIRE::Memory,
    pub input: WIRE::Input,
    pub sub_routine: WIRE::SubRoutineStack,
    pub control: WIRE::Control,
    pub runtime_flag: WIRE::RuntimeFlag,
    pub extend: WIRE::Extend,
}

impl<EXT: Default, MG: MemoryGetter> NewInterpreter<EthInterpreter<EXT, MG>> {
    /// Create new interpreter
    pub fn new(
        memory: Rc<RefCell<MG>>,
        bytecode: Bytecode,
        inputs: InputsImpl,
        is_static: bool,
        is_eof_init: bool,
        spec_id: SpecId,
        gas_limit: u64,
    ) -> Self {
        let runtime_flag = RuntimeFlags {
            spec_id,
            is_static,
            is_eof: bytecode.is_eof(),
            is_eof_init,
        };
        Self {
            bytecode: ExtBytecode::new(bytecode),
            stack: Stack::new(),
            return_data: ReturnDataImpl::default(),
            memory,
            input: inputs,
            sub_routine: SubRoutineImpl::default(),
            control: LoopControlImpl::new(gas_limit),
            runtime_flag,
            extend: EXT::default(),
        }
    }
}

pub struct EthInterpreter<EXT, MG = SharedMemory> {
    _phantom: core::marker::PhantomData<fn() -> (EXT, MG)>,
}

impl<EXT, MG: MemoryGetter> InterpreterWire for EthInterpreter<EXT, MG> {
    type Stack = Stack;
    type Memory = Rc<RefCell<MG>>;
    type Bytecode = ExtBytecode;
    type ReturnData = ReturnDataImpl;
    type Input = InputsImpl;
    type SubRoutineStack = SubRoutineImpl;
    type Control = LoopControlImpl;
    type RuntimeFlag = RuntimeFlags;
    type Extend = EXT;
}

pub trait InstructionProvider: Clone {
    type WIRE: InterpreterWire;
    type Host;

    fn new(ctx: &mut Self::Host) -> Self;

    fn table(&mut self) -> &[fn(&mut NewInterpreter<Self::WIRE>, &mut Self::Host); 256];
}

pub struct EthInstructionProvider<WIRE: InterpreterWire, HOST> {
    instruction_table: Rc<[Instruction<WIRE, HOST>; 256]>,
}

impl<WIRE, HOST> Clone for EthInstructionProvider<WIRE, HOST>
where
    WIRE: InterpreterWire,
{
    fn clone(&self) -> Self {
        Self {
            instruction_table: self.instruction_table.clone(),
        }
    }
}

impl<WIRE, HOST> InstructionProvider for EthInstructionProvider<WIRE, HOST>
where
    WIRE: InterpreterWire,
    HOST: Host,
{
    type WIRE = WIRE;
    type Host = HOST;

    fn new(_ctx: &mut Self::Host) -> Self {
        Self {
            instruction_table: Rc::new(crate::table::make_instruction_table::<WIRE, HOST>()),
        }
    }

    fn table(&mut self) -> &[fn(&mut NewInterpreter<Self::WIRE>, &mut Self::Host); 256] {
        self.instruction_table.as_ref()
    }
}

impl<IW: InterpreterWire> NewInterpreter<IW> {
    /// Executes the instruction at the current instruction pointer.
    ///
    /// Internally it will increment instruction pointer by one.
    #[inline]
    pub(crate) fn step<FN, H: Host + ?Sized>(&mut self, instruction_table: &[FN; 256], host: &mut H)
    where
        FN: Fn(&mut NewInterpreter<IW>, &mut H),
    {
        // Get current opcode.
        let opcode = self.bytecode.opcode();

        // SAFETY: In analysis we are doing padding of bytecode so that we are sure that last
        // byte instruction is STOP so we are safe to just increment program_counter bcs on last instruction
        // it will do noop and just stop execution of this contract
        self.bytecode.relative_jump(1);

        // execute instruction.
        (instruction_table[opcode as usize])(self, host)
    }

    /// Executes the interpreter until it returns or stops.
    pub fn run<FN, H: Host + ?Sized>(
        &mut self,
        instruction_table: &[FN; 256],
        host: &mut H,
    ) -> InterpreterAction
    where
        FN: Fn(&mut NewInterpreter<IW>, &mut H),
    {
        self.control
            .set_next_action(InterpreterAction::None, InstructionResult::Continue);

        // main loop
        while self.control.instruction_result().is_continue() {
            self.step(instruction_table, host);
        }

        // Return next action if it is some.
        let action = self.control.take_next_action();
        if action.is_some() {
            return action;
        }
        // If not, return action without output as it is a halt.
        InterpreterAction::Return {
            result: InterpreterResult {
                result: self.control.instruction_result(),
                // return empty bytecode
                output: Bytes::new(),
                gas: self.control.gas().clone(),
            },
        }
    }
}

/// The result of an interpreter operation.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub struct InterpreterResult {
    /// The result of the instruction execution.
    pub result: InstructionResult,
    /// The output of the instruction execution.
    pub output: Bytes,
    /// The gas usage information.
    pub gas: Gas,
}

impl InterpreterResult {
    /// Returns a new `InterpreterResult` with the given values.
    pub fn new(result: InstructionResult, output: Bytes, gas: Gas) -> Self {
        Self {
            result,
            output,
            gas,
        }
    }

    /// Returns whether the instruction result is a success.
    #[inline]
    pub const fn is_ok(&self) -> bool {
        self.result.is_ok()
    }

    /// Returns whether the instruction result is a revert.
    #[inline]
    pub const fn is_revert(&self) -> bool {
        self.result.is_revert()
    }

    /// Returns whether the instruction result is an error.
    #[inline]
    pub const fn is_error(&self) -> bool {
        self.result.is_error()
    }
}

// /// Resize the memory to the new size. Returns whether the gas was enough to resize the memory.
// #[inline(never)]
// #[cold]
// #[must_use]
// pub fn resize_memory(memory: &mut SharedMemory, gas: &mut Gas, new_size: usize) -> bool {
//     let new_words = num_words(new_size as u64);
//     let new_cost = gas::memory_gas(new_words);
//     let current_cost = memory.current_expansion_cost();
//     let cost = new_cost - current_cost;
//     let success = gas.record_cost(cost);
//     if success {
//         memory.resize((new_words as usize) * 32);
//     }
//     success
// }

#[cfg(test)]
mod tests {
    // use super::*;
    // use crate::{table::InstructionTable, DummyHost};

    // #[test]
    // fn object_safety() {
    //     let mut interp = Interpreter::new(Contract::default(), u64::MAX, false);
    //     interp.spec_id = SpecId::CANCUN;
    //     let mut host = crate::DummyHost::<DefaultEthereumWiring>::default();
    //     let table: &InstructionTable<DummyHost<DefaultEthereumWiring>> =
    //         &crate::table::make_instruction_table::<Interpreter, DummyHost<DefaultEthereumWiring>>(
    //         );
    //     let _ = interp.run(EMPTY_SHARED_MEMORY, table, &mut host);

    //     let host: &mut dyn Host<EvmWiringT = DefaultEthereumWiring> =
    //         &mut host as &mut dyn Host<EvmWiringT = DefaultEthereumWiring>;
    //     let table: &InstructionTable<dyn Host<EvmWiringT = DefaultEthereumWiring>> =
    //         &crate::table::make_instruction_table::<
    //             Interpreter,
    //             dyn Host<EvmWiringT = DefaultEthereumWiring>,
    //         >();
    //     let _ = interp.run(EMPTY_SHARED_MEMORY, table, host);
    // }
}
