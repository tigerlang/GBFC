pub mod x86_64;

use crate::error::Result;
use crate::ir::Op;
use crate::target::{Arch, Syscalls};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EofMode {
    Unchanged,
    Zero,
    MinusOne,
}

#[derive(Clone, Debug)]
pub struct CodegenOptions {
    pub eof: EofMode,
    pub bounds_check: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum EntryKind {
    Function,
    Process { tape_addr: u64, tape_size: u64 },
}

pub struct CodeImage {
    pub bytes: Vec<u8>,
    pub entry: usize,
}

pub trait CodeGen {
    fn generate(
        &self,
        ops: &[Op],
        opts: &CodegenOptions,
        sys: &Syscalls,
        entry: EntryKind,
    ) -> Result<CodeImage>;
}

pub fn for_arch(arch: Arch) -> Box<dyn CodeGen> {
    match arch {
        Arch::X86_64 => Box::new(x86_64::X86_64),
    }
}
