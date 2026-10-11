use crate::error::{err, Result};
use crate::ir::Op;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Bf,
    Bfpp,
}

pub fn parse(src: &[u8], lang: Lang) -> Result<Vec<Op>> {
    let mut stack: Vec<(Vec<Op>, usize, usize)> = Vec::new();
    let mut cur: Vec<Op> = Vec::new();
    let (mut line, mut col) = (1usize, 0usize);

    for &c in src {
        if c == b'\n' {
            line += 1;
            col = 0;
            continue;
        }
        col += 1;
        let bfpp = lang == Lang::Bfpp;
        match c {
            b'+' => add(&mut cur, 1),
            b'-' => add(&mut cur, 255),
            b'>' => mv(&mut cur, 1),
            b'<' => mv(&mut cur, -1),
            b'.' => cur.push(Op::Out { off: 0 }),
            b',' => cur.push(Op::In { off: 0 }),
            b'#' if bfpp => cur.push(Op::FOpen { off: 0 }),
            b':' if bfpp => cur.push(Op::FIn { off: 0 }),
            b';' if bfpp => cur.push(Op::FOut { off: 0 }),
            b'%' if bfpp => cur.push(Op::NetOpen { off: 0 }),
            b'^' if bfpp => cur.push(Op::NetOut { off: 0 }),
            b'!' if bfpp => cur.push(Op::NetIn { off: 0 }),
            b'[' => {
                let parent = std::mem::take(&mut cur);
                stack.push((parent, line, col));
            }
            b']' => {
                let Some((mut parent, _, _)) = stack.pop() else {
                    return err(format!("{line}:{col}: unmatched ']'"));
                };
                parent.push(Op::Loop { off: 0, body: std::mem::take(&mut cur) });
                cur = parent;
            }
            _ => {}
        }
    }
    if let Some(&(_, l, c)) = stack.last() {
        return err(format!("{l}:{c}: unmatched '['"));
    }
    Ok(cur)
}

fn add(cur: &mut Vec<Op>, n: u8) {
    if let Some(Op::Add { val, .. }) = cur.last_mut() {
        *val = val.wrapping_add(n);
        if *val == 0 {
            cur.pop();
        }
        return;
    }
    cur.push(Op::Add { off: 0, val: n });
}

fn mv(cur: &mut Vec<Op>, n: i32) {
    if let Some(Op::Move(m)) = cur.last_mut() {
        *m += n;
        if *m == 0 {
            cur.pop();
        }
        return;
    }
    cur.push(Op::Move(n));
}
