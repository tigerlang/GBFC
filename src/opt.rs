use crate::ir::Op;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

pub fn optimize(ops: Vec<Op>) -> Vec<Op> {
    fold_constants(&block(ops, true).0)
}

type Item = (Op, bool);

fn block(ops: Vec<Op>, at_start: bool) -> (Vec<Op>, bool) {
    let mut items: Vec<Item> = Vec::with_capacity(ops.len());
    let mut zero = at_start;
    let mut static_body = true;
    for op in ops {
        match op {
            Op::Loop { body, .. } => {
                if zero {
                    continue;
                }
                let (body, body_static) = block(body, false);
                static_body &= recognize(body, body_static, &mut items);
                zero = true;
            }
            other => {
                zero = false;
                items.push((other, false));
            }
        }
    }
    let out = normalize(items);
    static_body &= !out.iter().any(|op| matches!(op, Op::Move(_)));
    (out, static_body)
}

fn recognize(body: Vec<Op>, is_static: bool, items: &mut Vec<Item>) -> bool {
    if let Some(replacement) = as_mul_loop(&body) {
        items.extend(replacement.into_iter().map(|op| {
            let carry = matches!(op, Op::If { .. });
            (op, carry)
        }));
        return true;
    }
    items.push((Op::Loop { off: 0, body }, is_static));
    is_static
}

fn as_mul_loop(body: &[Op]) -> Option<Vec<Op>> {
    let mut step: Option<u8> = None;
    let mut targets: Vec<(i32, u8)> = Vec::new();
    for op in body {
        match *op {
            Op::Add { off: 0, val } => step = Some(val),
            Op::Add { off, val } => targets.push((off, val)),
            _ => return None,
        }
    }
    let step = step?;
    if step & 1 == 0 {
        return None;
    }
    let inverse = inv_odd(step.wrapping_neg());
    let mut effects = Vec::with_capacity(targets.len() + 1);
    for (dst, val) in targets {
        let factor = val.wrapping_mul(inverse);
        if factor != 0 {
            effects.push(Op::MulAdd { src: 0, dst, factor });
        }
    }
    effects.push(Op::Set { off: 0, val: 0 });
    if effects.len() == 1 {
        return Some(effects);
    }
    Some(vec![Op::If { off: 0, body: effects }])
}

fn inv_odd(value: u8) -> u8 {
    let mut x = value;
    for _ in 0..3 {
        x = x.wrapping_mul(2u8.wrapping_sub(value.wrapping_mul(x)));
    }
    x
}

enum Pending {
    Add(u8),
    Set(u8),
}

fn flush(pending: &mut BTreeMap<i32, Pending>, out: &mut Vec<Op>, last: Option<i32>) {
    let mut deferred = None;
    for (off, entry) in std::mem::take(pending) {
        let op = match entry {
            Pending::Add(0) => continue,
            Pending::Add(val) => Op::Add { off, val },
            Pending::Set(val) => Op::Set { off, val },
        };
        if Some(off) == last {
            deferred = Some(op);
        } else {
            out.push(op);
        }
    }
    out.extend(deferred);
}

fn normalize(items: Vec<Item>) -> Vec<Op> {
    let mut out = Vec::with_capacity(items.len());
    let mut pending: BTreeMap<i32, Pending> = BTreeMap::new();
    let mut off = 0i32;
    for (op, carry) in items {
        match op {
            Op::Add { off: o, val } => match pending.get_mut(&(o + off)) {
                Some(Pending::Add(sum)) => *sum = sum.wrapping_add(val),
                Some(Pending::Set(value)) => *value = value.wrapping_add(val),
                None => {
                    pending.insert(o + off, Pending::Add(val));
                }
            },
            Op::Set { off: o, val } => {
                pending.insert(o + off, Pending::Set(val));
            }
            Op::Move(n) => off += n,
            Op::MulAdd { src, dst, factor } => {
                flush(&mut pending, &mut out, None);
                out.push(Op::MulAdd { src: src + off, dst: dst + off, factor });
            }
            Op::In { off: o } => {
                flush(&mut pending, &mut out, None);
                out.push(Op::In { off: o + off });
            }
            Op::Out { off: o } => {
                flush(&mut pending, &mut out, None);
                out.push(Op::Out { off: o + off });
            }
            op @ Op::Write(_) => {
                flush(&mut pending, &mut out, None);
                out.push(op);
            }
            Op::Loop { body, .. } => {
                flush(&mut pending, &mut out, if carry { Some(off) } else { None });
                if !carry && off != 0 {
                    out.push(Op::Move(off));
                    off = 0;
                }
                out.push(Op::Loop { off, body });
            }
            Op::If { body, .. } => {
                flush(&mut pending, &mut out, Some(off));
                out.push(Op::If { off, body });
            }
        }
    }
    flush(&mut pending, &mut out, Some(0));
    if off != 0 {
        out.push(Op::Move(off));
    }
    out
}

pub fn fold_constants(ops: &[Op]) -> Vec<Op> {
    let state = State { default: Some(0), cells: HashMap::new(), origin: 0 };
    Folder { state, summaries: HashMap::new() }.block(ops, 0)
}

struct State {
    default: Option<u8>,
    cells: HashMap<i32, Option<u8>>,
    origin: i32,
}

impl State {
    fn get(&self, key: i32) -> Option<u8> {
        self.cells.get(&(key + self.origin)).copied().unwrap_or(self.default)
    }

    fn set(&mut self, key: i32, value: Option<u8>) {
        self.cells.insert(key + self.origin, value);
    }

    fn forget_all(&mut self) {
        self.default = None;
        self.cells.clear();
    }

    fn shift(&mut self, n: i32) {
        self.origin += n;
    }
}

#[derive(Default)]
struct Summary {
    written: BTreeSet<i32>,
    unbalanced: bool,
}

struct Folder {
    state: State,
    summaries: HashMap<usize, Rc<Summary>>,
}

impl Folder {
    fn summary(&mut self, node: &Op) -> Rc<Summary> {
        let key = node as *const Op as usize;
        if let Some(found) = self.summaries.get(&key) {
            return found.clone();
        }
        let mut summary = Summary::default();
        if let Op::Loop { off, body } | Op::If { off, body } = node {
            for child in body {
                match child {
                    Op::Loop { .. } | Op::If { .. } => {
                        let inner = self.summary(child);
                        summary.written.extend(inner.written.iter().copied());
                        summary.unbalanced |= inner.unbalanced;
                    }
                    Op::Add { off, .. } | Op::Set { off, .. } | Op::In { off } => {
                        summary.written.insert(*off);
                    }
                    Op::MulAdd { dst, .. } => {
                        summary.written.insert(*dst);
                    }
                    Op::Move(_) => summary.unbalanced = true,
                    Op::Out { .. } | Op::Write(_) => {}
                }
            }
            summary.written = summary.written.iter().map(|key| key + off).collect();
        }
        let summary = Rc::new(summary);
        self.summaries.insert(key, summary.clone());
        summary
    }

    fn block(&mut self, ops: &[Op], base: i32) -> Vec<Op> {
        let mut out: Vec<Op> = Vec::with_capacity(ops.len());
        for op in ops {
            match op {
                Op::Add { off, val } => {
                    let key = base + off;
                    if let Some(value) = self.state.get(key) {
                        self.state.set(key, Some(value.wrapping_add(*val)));
                    }
                    out.push(op.clone());
                }
                Op::Set { off, val } => {
                    let key = base + off;
                    if self.state.get(key) != Some(*val) {
                        self.state.set(key, Some(*val));
                        out.push(op.clone());
                    }
                }
                Op::MulAdd { src, dst, factor } => {
                    let dst_key = base + dst;
                    match self.state.get(base + src) {
                        Some(0) => {}
                        Some(value) => {
                            let delta = value.wrapping_mul(*factor);
                            if let Some(current) = self.state.get(dst_key) {
                                self.state.set(dst_key, Some(current.wrapping_add(delta)));
                            }
                            if delta != 0 {
                                out.push(Op::Add { off: *dst, val: delta });
                            }
                        }
                        None => {
                            self.state.set(dst_key, None);
                            out.push(op.clone());
                        }
                    }
                }
                Op::In { off } => {
                    self.state.set(base + off, None);
                    out.push(op.clone());
                }
                Op::Out { .. } | Op::Write(_) => out.push(op.clone()),
                Op::Move(n) => {
                    self.state.shift(*n);
                    out.push(op.clone());
                }
                Op::If { off, body } => {
                    let key = base + off;
                    match self.state.get(key) {
                        Some(0) => {}
                        Some(_) => {
                            let mut inner = self.block(body, key);
                            shift_offsets(&mut inner, *off);
                            out.extend(inner);
                        }
                        None => {
                            let written = self.summary(op).written.clone();
                            let before: Vec<(i32, Option<u8>)> =
                                written.iter().map(|k| (base + k, self.state.get(base + k))).collect();
                            let folded = self.block(body, key);
                            for (k, old) in before {
                                if k != key && self.state.get(k) != old {
                                    self.state.set(k, None);
                                }
                            }
                            if self.state.get(key) != Some(0) {
                                self.state.set(key, None);
                            }
                            if !folded.is_empty() {
                                out.push(Op::If { off: *off, body: folded });
                            }
                        }
                    }
                }
                Op::Loop { off, body } => {
                    let key = base + off;
                    if self.state.get(key) == Some(0) {
                        continue;
                    }
                    let summary = self.summary(op);
                    if summary.unbalanced {
                        self.state.forget_all();
                        let folded = self.block(body, key);
                        self.state.forget_all();
                        self.state.set(key, Some(0));
                        out.push(Op::Loop { off: *off, body: folded });
                        continue;
                    }
                    for k in &summary.written {
                        self.state.set(base + k, None);
                    }
                    let folded = self.block(body, key);
                    for k in &summary.written {
                        self.state.set(base + k, None);
                    }
                    self.state.set(key, Some(0));
                    out.push(Op::Loop { off: *off, body: folded });
                }
            }
        }
        out
    }
}

fn shift_offsets(ops: &mut [Op], delta: i32) {
    for op in ops {
        match op {
            Op::Add { off, .. } | Op::Set { off, .. } | Op::In { off } | Op::Out { off } => *off += delta,
            Op::MulAdd { src, dst, .. } => {
                *src += delta;
                *dst += delta;
            }
            Op::Loop { off, .. } | Op::If { off, .. } => *off += delta,
            Op::Move(_) | Op::Write(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use crate::testutil::{random_source, run, Lcg};

    #[test]
    fn hello_world() {
        let src = b"++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.";
        let raw = parse(src).unwrap();
        let optimized = optimize(raw.clone());
        assert_eq!(run(&optimized, b"", 1_000_000).unwrap(), b"Hello World!\n");
        assert_eq!(run(&raw, b"", 1_000_000).unwrap(), b"Hello World!\n");
    }

    #[test]
    fn patterns() {
        assert_eq!(optimize(parse(b"+++[-]").unwrap()), vec![]);
        assert_eq!(optimize(parse(b"[->+<].").unwrap()).len(), 1);
        assert_eq!(
            optimize(parse(b",[->+<]").unwrap()),
            vec![
                Op::In { off: 0 },
                Op::If {
                    off: 0,
                    body: vec![Op::MulAdd { src: 0, dst: 1, factor: 1 }, Op::Set { off: 0, val: 0 }]
                },
            ]
        );
        assert_eq!(optimize(parse(b",[>]").unwrap())[1], Op::Loop { off: 0, body: vec![Op::Move(1)] });
        let moves = |src: &[u8]| optimize(parse(src).unwrap()).iter().filter(|op| matches!(op, Op::Move(_))).count();
        assert_eq!(moves(b",>,[-<+>]<[>.<-]"), 0);
        assert_eq!(moves(b",>>,[>].<<<"), 2);
    }

    #[test]
    fn random_programs_equivalent() {
        let mut rng = Lcg(0x1234_5678_9abc_def0);
        let input = b"hello gbfc";
        let (mut checked, mut nontrivial) = (0, 0);
        for _ in 0..20000 {
            let src = random_source(&mut rng, b"+++");
            let raw = parse(&src).unwrap();
            let Some(expected) = run(&raw, input, 50_000) else { continue };
            let optimized = optimize(raw);
            let got = run(&optimized, input, 500_000).expect("optimized program must terminate");
            assert_eq!(got, expected, "program: {}", String::from_utf8_lossy(&src));
            checked += 1;
            if !expected.is_empty() {
                nontrivial += 1;
            }
        }
        assert!(checked > 2000 && nontrivial > 500, "checked={checked} nontrivial={nontrivial}");
    }
}
