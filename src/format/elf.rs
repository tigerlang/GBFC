use super::ExeLayout;
use crate::backend::CodeImage;
use crate::error::{err, Result};
use crate::target::Arch;

fn machine(arch: Arch) -> u16 {
    match arch {
        Arch::X86_64 => 62,
    }
}

struct W(Vec<u8>);
impl W {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    #[allow(clippy::too_many_arguments)]
    fn phdr(&mut self, ty: u32, flags: u32, off: u64, vaddr: u64, filesz: u64, memsz: u64, align: u64) {
        self.u32(ty);
        self.u32(flags);
        self.u64(off);
        self.u64(vaddr);
        self.u64(vaddr);
        self.u64(filesz);
        self.u64(memsz);
        self.u64(align);
    }
    #[allow(clippy::too_many_arguments)]
    fn shdr(&mut self, name: u32, ty: u32, flags: u64, addr: u64, off: u64, size: u64, align: u64) {
        self.u32(name);
        self.u32(ty);
        self.u64(flags);
        self.u64(addr);
        self.u64(off);
        self.u64(size);
        self.u32(0);
        self.u32(0);
        self.u64(align);
        self.u64(0);
    }
}

pub fn write(arch: Arch, layout: &ExeLayout, image: &CodeImage, tape_size: u64) -> Result<Vec<u8>> {
    const EH: u64 = 64;
    const PH: u64 = 56;
    const NPH: u64 = 3;
    const NSH: u64 = 4;

    let code = &image.bytes;
    let code_off = EH + NPH * PH;
    let code_end = code_off + code.len() as u64;
    if layout.image_base + code_end > layout.tape_addr {
        return err("program is too large for the executable layout");
    }
    let code_vaddr = layout.image_base + code_off;
    let shstr: &[u8] = b"\0.text\0.bss\0.shstrtab\0";
    let shoff = (code_end + shstr.len() as u64 + 7) & !7;

    let mut w = W(Vec::with_capacity(shoff as usize + (NSH * 64) as usize));

    w.0.extend_from_slice(&[0x7F, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    w.u16(2);
    w.u16(machine(arch));
    w.u32(1);
    w.u64(code_vaddr + image.entry as u64);
    w.u64(EH);
    w.u64(shoff);
    w.u32(0);
    w.u16(EH as u16);
    w.u16(PH as u16);
    w.u16(NPH as u16);
    w.u16(64);
    w.u16(NSH as u16);
    w.u16(3);

    w.phdr(1, 5, 0, layout.image_base, code_end, code_end, 0x1000);
    w.phdr(1, 6, 0, layout.tape_addr, 0, tape_size, 0x1000);
    w.phdr(0x6474_e551, 6, 0, 0, 0, 0, 16);

    w.0.extend_from_slice(code);
    w.0.extend_from_slice(shstr);
    w.0.resize(shoff as usize, 0);

    w.0.extend_from_slice(&[0u8; 64]);
    w.shdr(1, 1, 0x6, code_vaddr, code_off, code.len() as u64, 1);
    w.shdr(7, 8, 0x3, layout.tape_addr, code_end, tape_size, 4096);
    w.shdr(12, 3, 0, 0, code_end, shstr.len() as u64, 1);

    Ok(w.0)
}
