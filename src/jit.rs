use crate::backend::CodeImage;
use crate::error::{err, Result};
use crate::target::Target;

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::os::raw::{c_int, c_void};

    extern "C" {
        fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
        fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
        fn munmap(addr: *mut c_void, len: usize) -> c_int;
    }
    const PROT_READ: c_int = 1;
    const PROT_WRITE: c_int = 2;
    const PROT_EXEC: c_int = 4;
    const MAP_PRIVATE: c_int = 0x02;
    const MAP_ANONYMOUS: c_int = 0x20;

    pub struct JitCode {
        ptr: *mut c_void,
        len: usize,
        entry: usize,
    }

    impl JitCode {
        pub fn load(image: &CodeImage) -> Result<JitCode> {
            let len = image.bytes.len().max(1);
            unsafe {
                let p = mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
                if p as isize == -1 {
                    return err("mmap failed");
                }
                std::ptr::copy_nonoverlapping(image.bytes.as_ptr(), p as *mut u8, image.bytes.len());
                if mprotect(p, len, PROT_READ | PROT_EXEC) != 0 {
                    munmap(p, len);
                    return err("mprotect failed");
                }
                Ok(JitCode { ptr: p, len, entry: image.entry })
            }
        }

        pub unsafe fn call(&self, tape: *mut u8, size: usize) -> u32 {
            let f: extern "C" fn(*mut u8, usize) -> u32 =
                std::mem::transmute((self.ptr as *mut u8).add(self.entry));
            f(tape, size)
        }
    }

    impl Drop for JitCode {
        fn drop(&mut self) {
            unsafe {
                munmap(self.ptr, self.len);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;
    pub struct JitCode;
    impl JitCode {
        pub fn load(_: &CodeImage) -> Result<JitCode> {
            err("JIT is not supported on this host OS")
        }
        pub unsafe fn call(&self, _: *mut u8, _: usize) -> u32 {
            0
        }
    }
}

pub use imp::JitCode;

pub fn run(target: &Target, image: &CodeImage, tape_size: usize) -> Result<()> {
    if Target::host() != Some(*target) {
        return err(format!("cannot JIT-execute {target} code on this host"));
    }
    let code = JitCode::load(image)?;
    let mut tape = vec![0u8; tape_size];
    match unsafe { code.call(tape.as_mut_ptr(), tape_size) } {
        0 => Ok(()),
        1 => err("runtime error: tape pointer out of bounds"),
        _ => err("runtime error: I/O error"),
    }
}
