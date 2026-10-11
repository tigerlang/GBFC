use crate::ir::Op;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};

pub const N: i64 = 64;

struct State<'a> {
    tape: [u8; N as usize],
    p: i64,
    input: &'a [u8],
    read: usize,
    out: Vec<u8>,
    steps: u64,
    limit: u64,
    file: Option<std::fs::File>,
    sock: Option<TcpStream>,
}

impl State<'_> {
    fn at(&self, off: i32) -> usize {
        (self.p + off as i64).rem_euclid(N) as usize
    }

    fn text(&self, from: i32) -> Vec<u8> {
        let mut text = Vec::new();
        let mut at = from;
        while self.tape[self.at(at)] != 0 && text.len() < N as usize {
            text.push(self.tape[self.at(at)]);
            at += 1;
        }
        text
    }

    fn open_file(&mut self, off: i32) {
        let start = off + self.tape[self.at(off)] as i32;
        let path = String::from_utf8_lossy(&self.text(start)).to_string();
        self.file = None;
        self.tape[self.at(off)] = match OpenOptions::new().read(true).write(true).create(true).open(&path) {
            Ok(f) => {
                self.file = Some(f);
                0
            }
            Err(_) => 0xFF,
        };
    }

    fn open_socket(&mut self, off: i32) {
        let start = off + self.tape[self.at(off)] as i32;
        let host = self.text(start);
        let port_at = start + host.len() as i32 + 1;
        let port = u16::from(self.tape[self.at(port_at)]) << 8 | u16::from(self.tape[self.at(port_at + 1)]);
        let octets: Vec<u8> = String::from_utf8_lossy(&host).split('.').filter_map(|x| x.parse().ok()).collect();
        self.sock = None;
        self.tape[self.at(off)] = match octets.as_slice() {
            [a, b, c, d] => match TcpStream::connect((Ipv4Addr::new(*a, *b, *c, *d), port)) {
                Ok(s) => {
                    self.sock = Some(s);
                    0
                }
                Err(_) => 0xFF,
            },
            _ => 0xFF,
        };
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
                Op::FOpen { off } => self.open_file(base + off),
                Op::FClose => self.file = None,
                Op::FIn { off } => {
                    let cell = self.at(base + off);
                    let mut byte = [0u8; 1];
                    self.tape[cell] = self.file.as_mut().map_or(0, |f| if f.read_exact(&mut byte).is_ok() { byte[0] } else { 0 });
                }
                Op::FOut { off } => {
                    let byte = [self.tape[self.at(base + off)]];
                    if let Some(f) = &mut self.file {
                        let _ = f.write_all(&byte);
                    }
                }
                Op::NetOpen { off } => self.open_socket(base + off),
                Op::NetClose => self.sock = None,
                Op::NetIn { off } => {
                    let cell = self.at(base + off);
                    let mut byte = [0u8; 1];
                    self.tape[cell] = self.sock.as_mut().map_or(0, |s| if s.read_exact(&mut byte).is_ok() { byte[0] } else { 0 });
                }
                Op::NetOut { off } => {
                    let byte = [self.tape[self.at(base + off)]];
                    if let Some(s) = &mut self.sock {
                        let _ = s.write_all(&byte);
                    }
                }
            }
        }
        true
    }
}

pub fn run(ops: &[Op], input: &[u8], limit: u64) -> Option<Vec<u8>> {
    let mut state = State {
        tape: [0; N as usize],
        p: 0,
        input,
        read: 0,
        out: vec![],
        steps: 0,
        limit,
        file: None,
        sock: None,
    };
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
