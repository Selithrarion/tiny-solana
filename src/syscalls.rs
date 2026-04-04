use crate::runtime::{InvokeContext, ProgramError};
use solana_rbpf::error::StableResult;
use solana_rbpf::memory_region::{AccessType, MemoryMapping};
use solana_rbpf::program::{BuiltinFunction, BuiltinProgram, FunctionRegistry};
use solana_rbpf::{declare_builtin_function, vm::Config};
use std::fmt::Debug;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SyscallError {
    #[error("invalid string")]
    InvalidString,
    #[error("panic: {0} at {1}:{2}")]
    Panic(String, u64, u64),
}

impl From<SyscallError> for ProgramError {
    fn from(err: SyscallError) -> Self {
        ProgramError::InternalError(err.to_string())
    }
}

fn translate_string(mem: &mut MemoryMapping, addr: u64, len: u64) -> Result<String, SyscallError> {
    if len == 0 {
        return Ok(String::new());
    }

    let host_addr = match mem.map(AccessType::Load, addr, len) {
        StableResult::Ok(addr) => addr,
        StableResult::Err(_) => return Err(SyscallError::InvalidString),
    };

    let bytes = unsafe { std::slice::from_raw_parts(host_addr as *const u8, len as usize) };

    String::from_utf8(bytes.to_vec()).map_err(|_| SyscallError::InvalidString)
}

// sol_panic_()
declare_builtin_function!(
    SyscallPanic,
    fn rust(
        invoke_context: &mut InvokeContext,
        file_addr: u64,
        file_len: u64,
        line: u64,
        column: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        let file = translate_string(memory_mapping, file_addr, file_len)?;
        let msg = format!("panic in {file} at line {line}, column {column}");
        invoke_context.get_log_collector().borrow_mut().push(msg);
        Err(Box::new(SyscallError::Panic(file, line, column)))
    }
);

// sol_log_()
declare_builtin_function!(
    SyscallLog,
    fn rust(
        invoke_context: &mut InvokeContext,
        addr: u64,
        len: u64,
        _arg3: u64,
        _arg4: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        let message = translate_string(memory_mapping, addr, len)?;
        invoke_context
            .get_log_collector()
            .borrow_mut()
            .push(message);
        Ok(0)
    }
);

pub fn create_program_runtime<'a>() -> BuiltinProgram<InvokeContext<'a>> {
    let config = Config::default();
    let mut functions = FunctionRegistry::<BuiltinFunction<InvokeContext>>::default();

    let _ = functions.register_function_hashed(*b"sol_panic_", SyscallPanic::vm);
    let _ = functions.register_function_hashed(*b"sol_log_", SyscallLog::vm);
    // TODO: register all other syscalls
    BuiltinProgram::new_loader(config, functions)
}
