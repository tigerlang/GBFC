use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Add { off: i32, val: u8 },
    Set { off: i32, val: u8 },
    MulAdd { src: i32, dst: i32, factor: u8 },
    Move(i32),
    In { off: i32 },
    Out { off: i32 },
    /// Tests cell[p+off]; body offsets are relative to p+off, and a Move in the body shifts p.
    Loop { off: i32, body: Vec<Op> },
    /// Like `Loop` but runs the body at most once; the body never moves the pointer.
    If { off: i32, body: Vec<Op> },
    /// `Move(off)` followed by `while p != 0 { p += step }`; `step` is 1 or -1.
    Scan { off: i32, step: i8 },
    Write(Vec<u8>),
}

pub fn count(ops: &[Op]) -> usize {
    ops.iter()
        .map(|op| match op {
            Op::Loop { body, .. } | Op::If { body, .. } => 1 + count(body),
            _ => 1,
        })
        .sum()
}

pub fn dump(ops: &[Op]) -> String {
    let mut text = String::new();
    dump_into(ops, 0, &mut text);
    text
}

fn dump_into(ops: &[Op], depth: usize, text: &mut String) {
    let pad = "  ".repeat(depth);
    for op in ops {
        let _ = match op {
            Op::Add { off, val } => writeln!(text, "{pad}add   [{off:+}], {}", *val as i8),
            Op::Set { off, val } => writeln!(text, "{pad}set   [{off:+}], {val}"),
            Op::MulAdd { src, dst, factor } => {
                writeln!(text, "{pad}muladd [{dst:+}] += [{src:+}] * {}", *factor as i8)
            }
            Op::Move(n) => writeln!(text, "{pad}move  {n:+}"),
            Op::In { off } => writeln!(text, "{pad}in    [{off:+}]"),
            Op::Out { off } => writeln!(text, "{pad}out   [{off:+}]"),
            Op::Scan { off, step } => writeln!(text, "{pad}scan  [{off:+}] step {step:+}"),
            Op::Write(bytes) => {
                let preview: String = bytes
                    .iter()
                    .take(40)
                    .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
                    .collect();
                writeln!(text, "{pad}write {} bytes \"{preview}\"", bytes.len())
            }
            Op::Loop { off, body } | Op::If { off, body } => {
                let keyword = if matches!(op, Op::Loop { .. }) { "loop" } else { "if" };
                let _ = writeln!(text, "{pad}{keyword} [{off:+}] {{");
                dump_into(body, depth + 1, text);
                writeln!(text, "{pad}}}")
            }
        };
    }
}
