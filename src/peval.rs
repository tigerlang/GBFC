//! Compile-time execution of the program prefix; see README for limits and behaviour.

use crate::ir::Op;

pub struct Config {
    pub fuel: u64,
    pub tape_size: usize,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub steps: u64,
    pub finished: bool,
    pub static_output: usize,
}

const MAX_EVAL_TAPE: usize = 64 << 20;
const MAX_STATIC_OUT: usize = 1 << 20;
const MAX_INIT_CELLS: usize = 1 << 16;

enum Stop {
    Abort,
    Halt(Vec<Op>),
}

struct Ev {
    tape: Vec<u8>,
    p: i64,
    fuel: u64,
    out: Vec<u8>,
    budget: usize,
}

fn rebase(rest: Vec<Op>, off: i32) -> Vec<Op> {
    if off == 0 || rest.is_empty() {
        return rest;
    }
    let mut wrapped = Vec::with_capacity(rest.len() + 2);
    wrapped.push(Op::Move(off));
    wrapped.extend(rest);
    wrapped.push(Op::Move(-off));
    wrapped
}

impl Ev {
    #[inline(always)]
    fn cell(&mut self, offset: i64) -> Result<&mut u8, Stop> {
        let index = (self.p + offset) as usize;
        self.tape.get_mut(index).ok_or(Stop::Abort)
    }

    fn halt(&mut self, slice: &[Op]) -> Stop {
        let size = crate::ir::count(slice);
        if size > self.budget {
            return Stop::Abort;
        }
        self.budget -= size;
        Stop::Halt(slice.to_vec())
    }

    fn append(&mut self, rest: &mut Vec<Op>, slice: &[Op]) -> Result<(), Stop> {
        let size = crate::ir::count(slice);
        if size > self.budget {
            return Err(Stop::Abort);
        }
        self.budget -= size;
        rest.extend_from_slice(slice);
        Ok(())
    }

    fn exec(&mut self, ops: &[Op], base: i64) -> Result<(), Stop> {
        for (i, op) in ops.iter().enumerate() {
            if self.fuel == 0 {
                return Err(self.halt(&ops[i..]));
            }
            self.fuel -= 1;
            match op {
                Op::Add { off, val } => {
                    let cell = self.cell(base + *off as i64)?;
                    *cell = cell.wrapping_add(*val);
                }
                Op::Set { off, val } => *self.cell(base + *off as i64)? = *val,
                Op::MulAdd { src, dst, factor } => {
                    let value = *self.cell(base + *src as i64)?;
                    let cell = self.cell(base + *dst as i64)?;
                    *cell = cell.wrapping_add(value.wrapping_mul(*factor));
                }
                Op::Move(n) => {
                    self.p += *n as i64;
                    let logical = self.p + base;
                    if logical < 0 || logical as usize >= self.tape.len() {
                        return Err(Stop::Abort);
                    }
                }
                Op::In { .. } => return Err(Stop::Halt(ops[i..].to_vec())),
                Op::Out { off } => {
                    if self.out.len() >= MAX_STATIC_OUT {
                        return Err(self.halt(&ops[i..]));
                    }
                    let value = *self.cell(base + *off as i64)?;
                    self.out.push(value);
                }
                Op::Write(bytes) => {
                    if self.out.len() + bytes.len() > MAX_STATIC_OUT {
                        return Err(self.halt(&ops[i..]));
                    }
                    self.out.extend_from_slice(bytes);
                }
                Op::If { off, body } => {
                    let at = base + *off as i64;
                    if *self.cell(at)? != 0 {
                        match self.exec(body, at) {
                            Ok(()) => {}
                            Err(Stop::Halt(rest)) => {
                                let mut rest = rebase(rest, *off);
                                self.append(&mut rest, &ops[i + 1..])?;
                                return Err(Stop::Halt(rest));
                            }
                            Err(stop) => return Err(stop),
                        }
                    }
                }
                Op::Loop { off, body } => {
                    let at = base + *off as i64;
                    while *self.cell(at)? != 0 {
                        if self.fuel == 0 {
                            return Err(self.halt(&ops[i..]));
                        }
                        self.fuel -= 1;
                        match self.exec(body, at) {
                            Ok(()) => {}
                            Err(Stop::Halt(rest)) => {
                                let mut rest = rebase(rest, *off);
                                self.append(&mut rest, &ops[i..])?;
                                return Err(Stop::Halt(rest));
                            }
                            Err(stop) => return Err(stop),
                        }
                    }
                }
                Op::Scan { off, step } => {
                    self.p += *off as i64;
                    while *self.cell(base)? != 0 {
                        if self.fuel == 0 {
                            let mut rest = vec![Op::Scan { off: 0, step: *step }];
                            rest.extend_from_slice(&ops[i + 1..]);
                            return Err(self.halt(&rest));
                        }
                        self.fuel -= 1;
                        self.p += *step as i64;
                        let logical = self.p + base;
                        if logical < 0 || logical as usize >= self.tape.len() {
                            return Err(Stop::Abort);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn evaluate(ops: Vec<Op>, cfg: &Config) -> Vec<Op> {
    evaluate_with_stats(ops, cfg).0
}

pub fn evaluate_with_stats(ops: Vec<Op>, cfg: &Config) -> (Vec<Op>, Stats) {
    let none = Stats::default();
    if cfg.fuel == 0 || cfg.tape_size == 0 {
        return (ops, none);
    }
    let len = cfg.tape_size.min(MAX_EVAL_TAPE);
    let budget = 2 * crate::ir::count(&ops) + 4096;
    let mut ev = Ev { tape: vec![0; len], p: 0, fuel: cfg.fuel, out: Vec::new(), budget };

    let (residual, finished) = match ev.exec(&ops, 0) {
        Ok(()) => (Vec::new(), true),
        Err(Stop::Halt(rest)) => (rest, false),
        Err(Stop::Abort) => return (ops, none),
    };
    let steps = cfg.fuel - ev.fuel;
    if steps == 0 {
        return (ops, none);
    }
    let nonzero = if finished { 0 } else { ev.tape.iter().filter(|&&b| b != 0).count() };
    if nonzero > MAX_INIT_CELLS {
        return (ops, none);
    }

    let mut result = Vec::with_capacity(nonzero + residual.len() + 2);
    let static_output = ev.out.len();
    if !ev.out.is_empty() {
        result.push(Op::Write(std::mem::take(&mut ev.out)));
    }
    if !finished {
        for (i, &value) in ev.tape.iter().enumerate() {
            if value != 0 {
                result.push(Op::Set { off: i as i32, val: value });
            }
        }
        if ev.p != 0 {
            result.push(Op::Move(ev.p as i32));
        }
        result.extend(residual);
    }
    (result, Stats { steps, finished, static_output })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opt::{fold_constants, optimize};
    use crate::parser::parse;
    use crate::testutil::{random_source, run, Lcg};

    fn config(fuel: u64) -> Config {
        Config { fuel, tape_size: 64 }
    }

    #[test]
    fn hello_is_folded_into_one_write() {
        let src = b"++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.";
        let (ops, stats) =
            evaluate_with_stats(optimize(parse(src).unwrap()), &Config { fuel: 1_000_000, tape_size: 30000 });
        assert!(stats.finished);
        assert_eq!(ops, vec![Op::Write(b"Hello World!\n".to_vec())]);
    }

    #[test]
    fn stops_at_input_and_keeps_state() {
        let ops = evaluate(optimize(parse(b"+++>++.,[.,]").unwrap()), &config(1_000_000));
        assert_eq!(ops[0], Op::Write(vec![2]));
        assert_eq!(run(&ops, b"ab\0", 10_000).unwrap(), vec![2, b'a', b'b']);
    }

    #[test]
    fn out_of_tape_access_aborts_the_pass() {
        let ops = optimize(parse(b"+<+.").unwrap());
        assert_eq!(evaluate(ops.clone(), &config(1000)), ops);
    }

    #[test]
    fn random_programs_equivalent_for_any_fuel() {
        let mut rng = Lcg(0xfeed_beef_dead_c0de);
        let input = b"hello gbfc";
        let (mut checked, mut changed, mut halted_mid) = (0, 0, 0);
        for _ in 0..30000 {
            let src = random_source(&mut rng, b">>>>>>>>+++");
            let optimized = optimize(parse(&src).unwrap());
            let Some(expected) = run(&optimized, input, 50_000) else { continue };
            let fuel = (rng.next() % 3000) as u64 + 1;
            let (evaluated, stats) = evaluate_with_stats(optimized.clone(), &config(fuel));
            let folded = fold_constants(&evaluated);
            let got = run(&folded, input, 500_000).expect("residual program must terminate");
            assert_eq!(got, expected, "fuel={fuel} program: {}", String::from_utf8_lossy(&src));
            checked += 1;
            if evaluated != optimized {
                changed += 1;
                if !stats.finished {
                    halted_mid += 1;
                }
            }
        }
        assert!(changed > 1000 && halted_mid > 300, "checked={checked} changed={changed} halted_mid={halted_mid}");
    }
}
