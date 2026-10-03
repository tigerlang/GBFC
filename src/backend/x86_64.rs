//! x86-64 back end. Registers: rbx data pointer; r12 I/O buffers, r13/r14 output cursor/end,
//! r9/r10 input cursor/end (only with I/O); r15/rbp tape base/size (bounds checks);
//! r8d exit status (0 ok, 1 bounds trap, 2 I/O error).

use super::{CodeGen, CodeImage, CodegenOptions, EntryKind, EofMode};
use crate::error::{err, Result};
use crate::ir::Op;
use crate::target::Syscalls;
use std::collections::HashMap;

pub struct X86_64;

const BUF: u32 = 4096;
const TRAP_MSG: &[u8] = b"gbfc: runtime error: tape pointer out of bounds\n";
const IO_MSG: &[u8] = b"gbfc: runtime error: I/O error\n";

const CC_B: u8 = 0x2;
const CC_AE: u8 = 0x3;
const CC_E: u8 = 0x4;
const CC_NE: u8 = 0x5;
const CC_LE: u8 = 0xE;
const CC_G: u8 = 0xF;

#[derive(Clone, Copy)]
struct Cfg {
    eof: EofMode,
    sys: Syscalls,
    bounds: bool,
    out: bool,
    inp: bool,
}

impl Cfg {
    fn io(&self) -> bool {
        self.out || self.inp
    }
}

impl CodeGen for X86_64 {
    fn generate(
        &self,
        ops: &[Op],
        opts: &CodegenOptions,
        sys: &Syscalls,
        entry: EntryKind,
    ) -> Result<CodeImage> {
        let cfg = Cfg {
            eof: opts.eof,
            sys: *sys,
            bounds: opts.bounds_check,
            out: uses(ops, &|op| matches!(op, Op::Out { .. } | Op::Write(_))),
            inp: uses(ops, &|op| matches!(op, Op::In { .. })),
        };
        let mut asm = Asm::new(cfg);

        let stub_call = match entry {
            EntryKind::Function => None,
            EntryKind::Process { tape_addr, tape_size } => Some(asm.stub(tape_addr, tape_size)?),
        };
        if let Some(at) = stub_call {
            asm.patch_to(at, asm.buf.len());
        }

        asm.prologue();
        asm.ops(ops, 0);
        let end = asm.epilogue();

        let trap = asm.buf.len();
        asm.bytes(&[0x41, 0xB8, 1, 0, 0, 0]);
        asm.jmp_back(end);
        asm.finish(trap);

        Ok(CodeImage { bytes: asm.buf, entry: 0 })
    }
}

fn uses(ops: &[Op], wanted: &dyn Fn(&Op) -> bool) -> bool {
    ops.iter().any(|op| match op {
        Op::Loop { body, .. } | Op::If { body, .. } => uses(body, wanted),
        other => wanted(other),
    })
}

fn mul_run(ops: &[Op], start: usize) -> usize {
    let Op::MulAdd { src, .. } = ops[start] else { return 1 };
    ops[start..].iter().take_while(|op| matches!(op, Op::MulAdd { src: s, .. } if *s == src)).count()
}

fn flags_hot(ops: &[Op], i: usize) -> bool {
    let (Op::Loop { off, .. } | Op::If { off, .. }) = &ops[i] else { return false };
    i > 0 && matches!(&ops[i - 1], Op::Add { off: prev, val } if *val != 0 && prev == off)
}

fn scaled_sib(factor: u8) -> Option<(u8, bool)> {
    match factor {
        2 => Some((0x00, false)),
        3 => Some((0x40, false)),
        5 => Some((0x80, false)),
        9 => Some((0xC0, false)),
        254 => Some((0x00, true)),
        253 => Some((0x40, true)),
        251 => Some((0x80, true)),
        247 => Some((0xC0, true)),
        _ => None,
    }
}

struct Fwd {
    at: usize,
    short: bool,
}

struct Asm<'a> {
    buf: Vec<u8>,
    cfg: Cfg,
    lengths: HashMap<usize, usize>,
    statics: HashMap<usize, bool>,
    to_flush: Vec<usize>,
    to_put: Vec<usize>,
    to_write_all: Vec<usize>,
    to_refill: Vec<usize>,
    to_trap: Vec<usize>,
    to_data: Vec<(usize, usize)>,
    datas: Vec<&'a [u8]>,
}

impl<'a> Asm<'a> {
    fn new(cfg: Cfg) -> Self {
        Asm {
            buf: Vec::new(),
            cfg,
            lengths: HashMap::new(),
            statics: HashMap::new(),
            to_flush: Vec::new(),
            to_put: Vec::new(),
            to_write_all: Vec::new(),
            to_refill: Vec::new(),
            to_trap: Vec::new(),
            to_data: Vec::new(),
            datas: Vec::new(),
        }
    }

    fn byte(&mut self, b: u8) {
        self.buf.push(b);
    }

    fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn mem(&mut self, reg: u8, disp: i32) {
        if disp == 0 {
            self.byte((reg << 3) | 3);
        } else if let Ok(d) = i8::try_from(disp) {
            self.byte(0x40 | (reg << 3) | 3);
            self.byte(d as u8);
        } else {
            self.byte(0x80 | (reg << 3) | 3);
            self.u32(disp as u32);
        }
    }

    fn patch_to(&mut self, at: usize, target: usize) {
        let rel = target as i64 - (at as i64 + 4);
        self.buf[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
    }

    fn jcc_fwd(&mut self, cc: u8, short: bool) -> Fwd {
        if short {
            self.bytes(&[0x70 | cc, 0]);
            Fwd { at: self.buf.len() - 1, short }
        } else {
            self.bytes(&[0x0F, 0x80 | cc]);
            let at = self.buf.len();
            self.u32(0);
            Fwd { at, short }
        }
    }

    fn patch(&mut self, jump: Fwd) {
        if jump.short {
            let rel = self.buf.len() as i64 - (jump.at as i64 + 1);
            assert!((0..=127).contains(&rel), "rel8 forward jump out of range");
            self.buf[jump.at] = rel as u8;
        } else {
            self.patch_to(jump.at, self.buf.len());
        }
    }

    fn jcc_back(&mut self, cc: u8, target: usize, short: bool) {
        if short {
            let rel = target as i64 - (self.buf.len() as i64 + 2);
            assert!(rel >= -128, "rel8 backward jump out of range");
            self.bytes(&[0x70 | cc, rel as i8 as u8]);
        } else {
            self.bytes(&[0x0F, 0x80 | cc]);
            let rel = target as i64 - (self.buf.len() as i64 + 4);
            self.u32(rel as i32 as u32);
        }
    }

    fn jmp_back(&mut self, target: usize) {
        self.byte(0xE9);
        let rel = target as i64 - (self.buf.len() as i64 + 4);
        self.u32(rel as i32 as u32);
    }

    fn call_to(&mut self, fixups: fn(&mut Self) -> &mut Vec<usize>) {
        self.byte(0xE8);
        let at = self.buf.len();
        fixups(self).push(at);
        self.u32(0);
    }

    fn cmp_rax_eintr(&mut self) {
        let neg = -(self.cfg.sys.eintr as i32);
        self.bytes(&[0x48, 0x83, 0xF8, neg as i8 as u8]);
    }

    fn set_io_error(&mut self) {
        self.bytes(&[0x41, 0xB8, 2, 0, 0, 0]);
    }

    fn prologue(&mut self) {
        self.byte(0x53);
        if self.cfg.io() {
            self.bytes(&[0x41, 0x54, 0x41, 0x55, 0x41, 0x56]);
        }
        if self.cfg.bounds {
            self.bytes(&[0x41, 0x57, 0x55]);
        }
        self.bytes(&[0x45, 0x31, 0xC0]);
        self.bytes(&[0x48, 0x89, 0xFB]);
        if self.cfg.io() {
            self.bytes(&[0x48, 0x81, 0xEC]);
            self.u32(2 * BUF);
            self.bytes(&[0x49, 0x89, 0xE4]);
            self.bytes(&[0x49, 0x89, 0xE5]);
            self.bytes(&[0x4C, 0x8D, 0xB4, 0x24]);
            self.u32(BUF);
            self.bytes(&[0x4C, 0x8D, 0x8C, 0x24]);
            self.u32(BUF);
            self.bytes(&[0x4D, 0x89, 0xCA]);
        }
        if self.cfg.bounds {
            self.bytes(&[0x49, 0x89, 0xFF]);
            self.bytes(&[0x48, 0x89, 0xF5]);
        }
    }

    fn epilogue(&mut self) -> usize {
        let end = self.buf.len();
        if self.cfg.out {
            self.call_to(|s| &mut s.to_flush);
        }
        if self.cfg.io() {
            self.bytes(&[0x48, 0x81, 0xC4]);
            self.u32(2 * BUF);
        }
        if self.cfg.bounds {
            self.bytes(&[0x5D, 0x41, 0x5F]);
        }
        if self.cfg.io() {
            self.bytes(&[0x41, 0x5E, 0x41, 0x5D, 0x41, 0x5C]);
        }
        self.byte(0x5B);
        self.bytes(&[0x44, 0x89, 0xC0]);
        self.byte(0xC3);
        end
    }

    fn stub(&mut self, tape_addr: u64, tape_size: u64) -> Result<usize> {
        let Ok(tape) = u32::try_from(tape_addr) else {
            return err("tape address does not fit in 32 bits");
        };
        let (write, exit) = (self.cfg.sys.write, self.cfg.sys.exit);
        self.byte(0xBF);
        self.u32(tape);
        self.bytes(&[0x48, 0xBE]);
        self.u64(tape_size);
        self.byte(0xE8);
        let call = self.buf.len();
        self.u32(0);
        self.bytes(&[0x89, 0xC3]);
        self.bytes(&[0x85, 0xC0]);
        let ok = self.jcc_fwd(CC_E, true);
        self.byte(0xB8);
        self.u32(write);
        self.byte(0xBF);
        self.u32(2);
        self.bytes(&[0x48, 0x8D, 0x35]);
        let lea_bounds = self.buf.len();
        self.u32(0);
        self.byte(0xBA);
        self.u32(TRAP_MSG.len() as u32);
        self.bytes(&[0x83, 0xFB, 0x01]);
        let bounds = self.jcc_fwd(CC_E, true);
        self.bytes(&[0x48, 0x8D, 0x35]);
        let lea_io = self.buf.len();
        self.u32(0);
        self.byte(0xBA);
        self.u32(IO_MSG.len() as u32);
        self.patch(bounds);
        self.bytes(&[0x0F, 0x05]);
        self.patch(ok);
        self.byte(0xB8);
        self.u32(exit);
        self.bytes(&[0x89, 0xDF]);
        self.bytes(&[0x0F, 0x05]);
        let bounds_msg = self.buf.len();
        self.bytes(TRAP_MSG);
        let io_msg = self.buf.len();
        self.bytes(IO_MSG);
        self.patch_to(lea_bounds, bounds_msg);
        self.patch_to(lea_io, io_msg);
        Ok(call)
    }

    fn finish(&mut self, trap: usize) {
        let (mut flush, mut write_all, mut put, mut refill) = (0, 0, 0, 0);
        if self.cfg.out {
            write_all = self.buf.len();
            let top = self.buf.len();
            self.byte(0xB8);
            self.u32(self.cfg.sys.write);
            self.byte(0xBF);
            self.u32(1);
            self.bytes(&[0x0F, 0x05]);
            self.cmp_rax_eintr();
            self.jcc_back(CC_E, top, true);
            self.bytes(&[0x48, 0x85, 0xC0]);
            let failed = self.jcc_fwd(CC_LE, true);
            self.bytes(&[0x48, 0x01, 0xC6]);
            self.bytes(&[0x48, 0x29, 0xC2]);
            self.jcc_back(CC_NE, top, true);
            self.byte(0xC3);
            self.patch(failed);
            self.set_io_error();
            self.byte(0xC3);

            flush = self.buf.len();
            self.bytes(&[0x4C, 0x89, 0xEA]);
            self.bytes(&[0x4C, 0x29, 0xE2]);
            self.bytes(&[0x70 | CC_E, 11]);
            self.bytes(&[0x4C, 0x89, 0xE6]);
            self.bytes(&[0x4D, 0x89, 0xE5]);
            self.jmp_back(write_all);
            self.byte(0xC3);

            put = self.buf.len();
            self.bytes(&[0x41, 0x88, 0x45, 0x00]);
            self.bytes(&[0x49, 0xFF, 0xC5]);
            self.bytes(&[0x4D, 0x39, 0xF5]);
            self.bytes(&[0x70 | CC_B, 5]);
            self.jmp_back(flush);
            self.byte(0xC3);
        }
        if self.cfg.inp {
            refill = self.buf.len();
            if self.cfg.out {
                self.byte(0xE8);
                let rel = flush as i64 - (self.buf.len() as i64 + 4);
                self.u32(rel as i32 as u32);
            }
            let top = self.buf.len();
            self.byte(0xB8);
            self.u32(self.cfg.sys.read);
            self.bytes(&[0x31, 0xFF]);
            self.bytes(&[0x49, 0x8D, 0xB4, 0x24]);
            self.u32(BUF);
            self.byte(0xBA);
            self.u32(BUF);
            self.bytes(&[0x0F, 0x05]);
            self.cmp_rax_eintr();
            self.jcc_back(CC_E, top, true);
            self.bytes(&[0x48, 0x85, 0xC0]);
            let filled = self.jcc_fwd(CC_G, true);
            let eof = self.jcc_fwd(CC_E, true);
            self.set_io_error();
            self.patch(eof);
            self.byte(0xC3);
            self.patch(filled);
            self.bytes(&[0x4D, 0x8D, 0x8C, 0x24]);
            self.u32(BUF);
            self.bytes(&[0x4D, 0x8D, 0x14, 0x01]);
            self.byte(0xC3);
        }
        let datas = std::mem::take(&mut self.datas);
        let mut data_pos = Vec::with_capacity(datas.len());
        for data in &datas {
            data_pos.push(self.buf.len());
            self.buf.extend_from_slice(data);
        }
        for (fixups, target) in [
            (std::mem::take(&mut self.to_trap), trap),
            (std::mem::take(&mut self.to_flush), flush),
            (std::mem::take(&mut self.to_put), put),
            (std::mem::take(&mut self.to_write_all), write_all),
            (std::mem::take(&mut self.to_refill), refill),
        ] {
            for at in fixups {
                self.patch_to(at, target);
            }
        }
        for (at, index) in std::mem::take(&mut self.to_data) {
            self.patch_to(at, data_pos[index]);
        }
    }

    fn measure(&self, build: impl FnOnce(&mut Asm<'a>)) -> usize {
        let mut scratch = Asm::new(self.cfg);
        build(&mut scratch);
        scratch.buf.len()
    }

    fn ops_len(&mut self, ops: &'a [Op], base: i32) -> usize {
        let (mut total, mut i) = (0, 0);
        while i < ops.len() {
            if matches!(ops[i], Op::MulAdd { .. }) {
                let run = mul_run(ops, i);
                total += self.measure(|s| s.mul_group(&ops[i..i + run], base));
                i += run;
            } else {
                total += self.op_len(&ops[i], base, flags_hot(ops, i));
                i += 1;
            }
        }
        total
    }

    fn op_len(&mut self, op: &'a Op, base: i32, hot: bool) -> usize {
        let (off, body) = match op {
            Op::Loop { off, body } | Op::If { off, body } => (*off, body),
            _ => return self.measure(|s| s.op(op, base, false)),
        };
        let key = op as *const Op as usize;
        if let Some(&len) = self.lengths.get(&key) {
            return len;
        }
        let at = base + off;
        let body_len = self.ops_len(body, at);
        let head = if hot {
            0
        } else {
            self.measure(|s| {
                s.check(at, false);
                s.cmp0(at);
            })
        };
        let len = if matches!(op, Op::Loop { .. }) {
            let short = loop_short(body_len);
            let tail = if self.reuses_flags(body) { 0 } else { self.measure(|s| s.cmp0(at)) };
            head + jcc_len(short) + body_len + tail + jcc_len(short)
        } else {
            head + jcc_len(if_short(body_len)) + body_len
        };
        self.lengths.insert(key, len);
        len
    }

    fn is_static(&mut self, body: &'a [Op]) -> bool {
        let key = body.as_ptr() as usize;
        if let Some(&known) = self.statics.get(&key) {
            return known;
        }
        let mut result = true;
        for op in body {
            result = match op {
                Op::Move(_) => false,
                Op::Loop { body: inner, .. } | Op::If { body: inner, .. } => self.is_static(inner),
                _ => true,
            };
            if !result {
                break;
            }
        }
        self.statics.insert(key, result);
        result
    }

    fn reuses_flags(&mut self, body: &'a [Op]) -> bool {
        matches!(body.last(), Some(Op::Add { off: 0, val }) if *val != 0) && self.is_static(body)
    }

    fn ops(&mut self, ops: &'a [Op], base: i32) {
        let mut i = 0;
        while i < ops.len() {
            if matches!(ops[i], Op::MulAdd { .. }) {
                let run = mul_run(ops, i);
                self.mul_group(&ops[i..i + run], base);
                i += run;
            } else {
                self.op(&ops[i], base, flags_hot(ops, i));
                i += 1;
            }
        }
    }

    fn op(&mut self, op: &'a Op, base: i32, hot: bool) {
        match *op {
            Op::Add { off, val } => {
                self.check(base + off, false);
                self.add(base + off, val);
            }
            Op::Set { off, val } => {
                self.check(base + off, false);
                self.byte(0xC6);
                self.mem(0, base + off);
                self.byte(val);
            }
            Op::MulAdd { .. } => self.mul_group(std::slice::from_ref(op), base),
            Op::Move(n) => {
                self.add_ptr(n);
                if n != 0 {
                    self.check(base, true);
                }
            }
            Op::In { off } => self.input(base + off),
            Op::Out { off } => {
                self.check(base + off, false);
                self.byte(0x8A);
                self.mem(0, base + off);
                self.call_to(|s| &mut s.to_put);
            }
            Op::Write(ref bytes) => {
                if bytes.is_empty() {
                    return;
                }
                self.call_to(|s| &mut s.to_flush);
                self.bytes(&[0x48, 0x8D, 0x35]);
                self.datas.push(bytes);
                self.to_data.push((self.buf.len(), self.datas.len() - 1));
                self.u32(0);
                self.byte(0xBA);
                self.u32(bytes.len() as u32);
                self.call_to(|s| &mut s.to_write_all);
            }
            Op::If { off, ref body } => {
                let at = base + off;
                let body_len = self.ops_len(body, at);
                if !hot {
                    self.check(at, false);
                    self.cmp0(at);
                }
                let skip = self.jcc_fwd(CC_E, if_short(body_len));
                self.ops(body, at);
                self.patch(skip);
            }
            Op::Loop { off, ref body } => {
                let at = base + off;
                let short = loop_short(self.ops_len(body, at));
                let reuse = self.reuses_flags(body);
                if !hot {
                    self.check(at, false);
                    self.cmp0(at);
                }
                let skip = self.jcc_fwd(CC_E, short);
                let top = self.buf.len();
                self.ops(body, at);
                if !reuse {
                    self.cmp0(at);
                }
                self.jcc_back(CC_NE, top, short);
                self.patch(skip);
            }
        }
    }

    fn check(&mut self, at: i32, force: bool) {
        if !self.cfg.bounds || (at == 0 && !force) {
            return;
        }
        self.bytes(&[0x48, 0x8D]);
        self.mem(2, at);
        self.bytes(&[0x4C, 0x29, 0xFA]);
        self.bytes(&[0x48, 0x39, 0xEA]);
        self.bytes(&[0x0F, 0x80 | CC_AE]);
        self.to_trap.push(self.buf.len());
        self.u32(0);
    }

    fn add(&mut self, at: i32, val: u8) {
        match val {
            0 => {}
            1 => {
                self.byte(0xFE);
                self.mem(0, at);
            }
            255 => {
                self.byte(0xFE);
                self.mem(1, at);
            }
            v => {
                self.byte(0x80);
                self.mem(0, at);
                self.byte(v);
            }
        }
    }

    fn add_ptr(&mut self, n: i32) {
        if n == 0 {
            return;
        }
        if let Ok(d) = i8::try_from(n) {
            self.bytes(&[0x48, 0x83, 0xC3, d as u8]);
        } else {
            self.bytes(&[0x48, 0x81, 0xC3]);
            self.u32(n as u32);
        }
    }

    fn cmp0(&mut self, at: i32) {
        self.byte(0x80);
        self.mem(7, at);
        self.byte(0);
    }

    fn mul_group(&mut self, group: &[Op], base: i32) {
        let Some(Op::MulAdd { src, .. }) = group.first() else { return };
        self.check(base + src, false);
        self.bytes(&[0x0F, 0xB6]);
        self.mem(0, base + src);
        for op in group {
            let Op::MulAdd { dst, factor, .. } = *op else { continue };
            if factor == 0 {
                continue;
            }
            self.check(base + dst, false);
            let (reg, subtract) = match factor {
                1 => (0, false),
                255 => (0, true),
                f => {
                    let subtract = match scaled_sib(f) {
                        Some((sib, subtract)) => {
                            self.bytes(&[0x8D, 0x0C, sib]);
                            subtract
                        }
                        None => {
                            self.bytes(&[0x6B, 0xC8, f]);
                            false
                        }
                    };
                    (1, subtract)
                }
            };
            self.byte(if subtract { 0x28 } else { 0x00 });
            self.mem(reg, base + dst);
        }
    }

    fn input(&mut self, at: i32) {
        self.check(at, false);
        self.bytes(&[0x4D, 0x39, 0xD1]);
        let have = self.jcc_fwd(CC_B, true);
        self.call_to(|s| &mut s.to_refill);
        self.bytes(&[0x4D, 0x39, 0xD1]);
        let (refilled, done) = match self.cfg.eof {
            EofMode::Unchanged => (None, self.jcc_fwd(CC_AE, true)),
            mode => {
                let refilled = self.jcc_fwd(CC_B, true);
                self.byte(0xC6);
                self.mem(0, at);
                self.byte(if mode == EofMode::Zero { 0x00 } else { 0xFF });
                (Some(refilled), self.jcc_fwd_unconditional())
            }
        };
        self.patch(have);
        if let Some(jump) = refilled {
            self.patch(jump);
        }
        self.bytes(&[0x41, 0x8A, 0x01]);
        self.bytes(&[0x49, 0xFF, 0xC1]);
        self.byte(0x88);
        self.mem(0, at);
        self.patch(done);
    }

    fn jcc_fwd_unconditional(&mut self) -> Fwd {
        self.bytes(&[0xEB, 0]);
        Fwd { at: self.buf.len() - 1, short: true }
    }
}

fn jcc_len(short: bool) -> usize {
    if short {
        2
    } else {
        6
    }
}

fn loop_short(body_len: usize) -> bool {
    body_len + 9 <= 127
}

fn if_short(body_len: usize) -> bool {
    body_len <= 127
}
