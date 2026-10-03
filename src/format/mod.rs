pub mod elf;

use crate::backend::CodeImage;
use crate::error::{err, Result};
use crate::target::{Os, Target};

#[derive(Clone, Copy, Debug)]
pub struct ExeLayout {
    pub image_base: u64,
    pub tape_addr: u64,
}

pub fn exe_layout(target: &Target) -> ExeLayout {
    match target.os {
        Os::Linux => ExeLayout { image_base: 0x40_0000, tape_addr: 0x4000_0000 },
    }
}

pub fn write_executable(target: &Target, image: &CodeImage, tape_size: u64) -> Result<Vec<u8>> {
    if tape_size == 0 || tape_size > (1 << 40) {
        return err("tape size out of range");
    }
    match target.os {
        Os::Linux => elf::write(target.arch, &exe_layout(target), image, tape_size),
    }
}
