use crate::ir::Op;

pub const N: i64 = 64;

struct State<'a> {
    tape: [u8; N as usize],
    p: i64,
    input: &'a [u8],
    read: usize,
    out: Vec<u8>,
    steps: u64,
    limit: u64,
}

impl State<'_> {
    fn at(&self, off: i32) -> usize {
        (self.p + off as i64).rem_euclid(N) as usize
    }

    fn tick(&mut self) -> bool {
        self.steps += 1;
        self.steps <= self.limit
    }

    fn exec(&mut self, ops: &[Op], base: i32) -> bool {
        for op in ops {
            if !self.tick() {
                return false;
            }
            match op {
                Op::Add { off, val } => {
                    let i = self.at(base + off);
                    self.tape[i] = self.tape[i].wrapping_add(*val);
                }
                Op::Set { off, val } => {
                    let i = self.at(base + off);
                    self.tape[i] = *val;
                }
                Op::MulAdd { src, dst, factor } => {
                    let value = self.tape[self.at(base + src)];
                    let i = self.at(base + dst);
                    self.tape[i] = self.tape[i].wrapping_add(value.wrapping_mul(*factor));
                }
                Op::Move(n) => self.p = self.at(*n) as i64,
                Op::In { off } => {
                    if self.read < self.input.len() {
                        let i = self.at(base + off);
                        self.tape[i] = self.input[self.read];
                        self.read += 1;
                    }
                }
                Op::Out { off } => {
                    let value = self.tape[self.at(base + off)];
                    self.out.push(value);
                }
                Op::Write(bytes) => self.out.extend_from_slice(bytes),
                Op::Loop { off, body } => {
                    while self.tape[self.at(base + off)] != 0 {
                        if !self.tick() || !self.exec(body, base + off) {
                            return false;
                        }
                    }
                }
                Op::If { off, body } => {
                    if self.tape[self.at(base + off)] != 0 && !self.exec(body, base + off) {
                        return false;
                    }
                }
                Op::Scan { off, step } => {
                    self.p = self.at(*off) as i64;
                    while self.tape[self.at(base)] != 0 {
                        if !self.tick() {
                            return false;
                        }
                        self.p = self.at(*step as i32) as i64;
                    }
                }
            }
        }
        true
    }
}

pub fn run(ops: &[Op], input: &[u8], limit: u64) -> Option<Vec<u8>> {
    let mut state = State { tape: [0; N as usize], p: 0, input, read: 0, out: vec![], steps: 0, limit };
    state.exec(ops, 0).then_some(state.out)
}

pub struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as usize
    }
}

pub fn random_source(rng: &mut Lcg, prefix: &[u8]) -> Vec<u8> {
    let alphabet = b"+++---<<>>[]..,";
    let len = 10 + rng.next() % 120;
    let mut src = prefix.to_vec();
    let mut depth = 0;
    for _ in 0..len {
        let c = alphabet[rng.next() % alphabet.len()];
        match c {
            b'[' => depth += 1,
            b']' if depth == 0 => continue,
            b']' => depth -= 1,
            _ => {}
        }
        src.push(c);
    }
    src.extend(std::iter::repeat(b']').take(depth));
    src
}
