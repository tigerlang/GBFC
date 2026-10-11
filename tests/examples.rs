#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const MODES: &[&[&str]] = &[
    &["-O0"],
    &["-O0", "--bounds-check"],
    &["-O1", "--fuel", "0"],
    &["-O1", "--fuel", "0", "--bounds-check"],
    &["-O1"],
    &["-O1", "--bounds-check"],
];

fn gbfc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_gbfc"))
}

fn run_with_stdin(mut cmd: Command, input: &[u8]) -> Output {
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    out
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("gbfc-test-{}-{name}", std::process::id()))
}

fn both(src: &std::path::Path, flags: &[&str], input: &[u8], tag: &str) -> [Output; 2] {
    let mut c = gbfc();
    c.arg("-j").args(flags).arg(src);
    let jit = run_with_stdin(c, input);
    let exe = tmp(tag);
    let st = gbfc().args(flags).arg(src).arg("-o").arg(&exe).output().unwrap();
    assert!(st.status.success(), "compile failed: {}", String::from_utf8_lossy(&st.stderr));
    let aot = run_with_stdin(Command::new(&exe), input);
    let _ = std::fs::remove_file(&exe);
    [jit, aot]
}

#[test]
fn examples_in_all_modes() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let src = entry.unwrap().path();
        if src.extension().and_then(|e| e.to_str()) != Some("b") {
            continue;
        }
        let input = std::fs::read(src.with_extension("in")).unwrap_or_default();
        let expected = std::fs::read(src.with_extension("out")).unwrap();
        let stem = src.file_stem().unwrap().to_str().unwrap().to_string();
        for (i, mode) in MODES.iter().enumerate() {
            let [jit, aot] = both(&src, mode, &input, &format!("{stem}{i}"));
            for (kind, out) in [("jit", jit), ("aot", aot)] {
                assert!(out.status.success(), "{stem} {kind} {mode:?}: {:?}", out.status);
                assert_eq!(out.stdout, expected, "{stem} {kind} {mode:?}");
            }
        }
        n += 1;
    }
    assert!(n >= 4, "expected example programs, found {n}");
}

#[test]
fn eof_modes() {
    let src = tmp("eof.b");
    std::fs::write(&src, "+,.").unwrap();
    for (mode, want) in [("unchanged", 1u8), ("zero", 0), ("minus1", 255)] {
        for fuel in ["0", "1000"] {
            let mut c = gbfc();
            c.args(["-j", "--eof", mode, "--fuel", fuel]).arg(&src);
            assert_eq!(run_with_stdin(c, b"").stdout, vec![want], "--eof {mode} --fuel {fuel}");
        }
    }
    let _ = std::fs::remove_file(&src);
}

#[test]
fn syntax_errors_are_reported() {
    for (code, msg) in [("[[+]", "1:1: unmatched '['"), ("+\n  ]", "2:3: unmatched ']'")] {
        let src = tmp("bad.b");
        std::fs::write(&src, code).unwrap();
        let out = gbfc().arg("-j").arg(&src).output().unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(msg), "stderr was: {stderr}");
        let _ = std::fs::remove_file(&src);
    }
}

#[test]
fn io_errors_set_exit_status() {
    let printer = tmp("io-print.b");
    let reader = tmp("io-read.b");
    std::fs::write(&printer, "+++++++[>++++++++<-]>.").unwrap();
    std::fs::write(&reader, ",.").unwrap();
    let full = || std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();

    for fuel in ["0", "1000"] {
        let exe = tmp(&format!("io-print{fuel}"));
        assert!(gbfc().args(["--fuel", fuel]).arg(&printer).arg("-o").arg(&exe).status().unwrap().success());
        let out = Command::new(&exe).stdout(full()).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "write failure, fuel {fuel}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("I/O error"));
        let _ = std::fs::remove_file(&exe);
    }
    let jit = gbfc().args(["-j", "--fuel", "0"]).arg(&printer).stdout(full()).output().unwrap();
    assert!(!jit.status.success());

    let exe = tmp("io-read");
    assert!(gbfc().arg(&reader).arg("-o").arg(&exe).status().unwrap().success());
    let directory = std::fs::File::open("/").unwrap();
    let out = Command::new(&exe).stdin(directory).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "read failure");
    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(&printer);
    let _ = std::fs::remove_file(&reader);
}

#[test]
fn compile_time_execution_folds_program() {
    let hello = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/hello.b");
    let ir = |extra: &[&str]| {
        let o = gbfc().args(["--emit", "ir"]).args(extra).arg(&hello).output().unwrap();
        String::from_utf8(o.stdout).unwrap()
    };
    let folded = ir(&[]);
    assert!(folded.contains("write 13 bytes") && !folded.contains("loop"), "{folded}");
    assert!(ir(&["--fuel", "0"]).contains("loop"));
}

#[test]
fn buffered_output_boundaries() {
    let src = tmp("cat.b");
    std::fs::write(&src, ",[.,]").unwrap();
    for len in [0usize, 1, 4095, 4096, 4097, 8192, 8193, 1 << 20] {
        let input: Vec<u8> = (0..len).map(|i| (i % 255) as u8 + 1).collect();
        for mode in [&["-O1"][..], &["-O0"][..]] {
            let [jit, aot] = both(&src, mode, &input, &format!("catb{len}"));
            assert_eq!(jit.stdout, input, "jit len={len}");
            assert_eq!(aot.stdout, input, "aot len={len}");
        }
    }
    let _ = std::fs::remove_file(&src);
}

#[test]
fn short_and_near_jumps() {
    for k in 1..=45usize {
        let looped = format!("+++[->{}{}.]", "+>".repeat(k).trim_end_matches('>'), "<".repeat(k));
        let iff = format!("+++[-{}{}]{}", ">+".repeat(k), "<".repeat(k), ">.".repeat(k));
        let counted = format!("+++[>{}<-]", ".".repeat(k));
        let hot = format!("+++++-[>{}<-]", ".".repeat(k));
        for (name, code, want) in [
            ("loop", looped, vec![2u8, 1, 0]),
            ("if", iff, vec![3u8; k]),
            ("counted", counted, vec![0u8; 3 * k]),
            ("hot", hot, vec![0u8; 4 * k]),
        ] {
            let src = tmp(&format!("jump-{name}{k}.b"));
            std::fs::write(&src, &code).unwrap();
            for flags in [&["-O0"][..], &["-O1", "--fuel", "0"], &["-O1", "--fuel", "0", "--bounds-check"]] {
                let [jit, aot] = both(&src, flags, b"", &format!("jump-{name}{k}"));
                assert_eq!(jit.stdout, want, "{name} k={k} jit {flags:?}");
                assert_eq!(aot.stdout, want, "{name} k={k} aot {flags:?}");
            }
            let _ = std::fs::remove_file(&src);
        }
    }
}

#[test]
fn scan_loops() {
    let mut n = 0;
    for len in [1usize, 2, 15, 16, 17, 33] {
        let forward = format!("{}[>].", "+>".repeat(len) + &"<".repeat(len));
        let backward = format!("{}[<].", ">+".repeat(len));
        for (name, code) in [("forward", forward), ("backward", backward)] {
            let src = tmp(&format!("scan-{name}{len}.b"));
            std::fs::write(&src, &code).unwrap();
            for mode in MODES {
                let [jit, aot] = both(&src, mode, b"", &format!("scan-{name}{len}"));
                assert_eq!(jit.stdout, vec![0u8], "{name} {len} jit {mode:?}");
                assert_eq!(aot.stdout, vec![0u8], "{name} {len} aot {mode:?}");
            }
            let _ = std::fs::remove_file(&src);
            n += 1;
        }
    }
    let cases: &[(&str, &[u8])] = &[
        ("+>>+[<]+.", &[1]),
        ("+>+>+<<[>]+.>>+++[<]+.", &[1, 1]),
        ("+>+>+<<[>]+[->+<]>.", &[1]),
    ];
    for (i, (code, want)) in cases.iter().enumerate() {
        let src = tmp(&format!("scan{i}.b"));
        std::fs::write(&src, code).unwrap();
        for mode in MODES {
            let [jit, aot] = both(&src, mode, b"", &format!("scan{i}"));
            assert_eq!(jit.stdout, *want, "{code} jit {mode:?}");
            assert_eq!(aot.stdout, *want, "{code} aot {mode:?}");
        }
        let _ = std::fs::remove_file(&src);
        n += 1;
    }
    assert!(n > 10, "checked {n} scan programs");
}

#[test]
fn brainfuck_pp_files() {
    let name = tmp("bfpp-file.b");
    let path = tmp("bfpp-file.t");
    let _ = std::fs::remove_file(&path);
    let mut src = push_bytes(path.to_str().unwrap().as_bytes(), 5);
    src.push('#');
    src += &"+".repeat(b'A' as usize);
    src.push(';');
    src.push('#');
    src.push_str("[-]+++++#:.");
    std::fs::write(&name, src).unwrap();
    for mode in [&["-bfpp"][..], &["-bf++", "-O0"][..], &["-bfpp", "--bounds-check"][..]] {
        let [jit, aot] = both(&name, mode, b"", "bfpp-file");
        assert_eq!(jit.stdout, vec![b'A'], "jit {mode:?}");
        assert_eq!(aot.stdout, vec![b'A'], "aot {mode:?}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"A");
    let _ = std::fs::remove_file(&name);
    let _ = std::fs::remove_file(&path);
}

fn push_bytes(bytes: &[u8], distance: u32) -> String {
    let mut src = format!("{}{}", "+".repeat(distance as usize), ">".repeat(distance as usize));
    for &b in bytes {
        src += &"+".repeat(b as usize);
        src.push('>');
    }
    src += &"<".repeat(distance as usize + bytes.len());
    src
}

#[test]
fn brainfuck_pp_open_failure() {
    let name = tmp("bfpp-missing.b");
    let path = tmp("no/such/dir/file.t");
    std::fs::write(&name, format!("{}#.", push_bytes(path.to_str().unwrap().as_bytes(), 5))).unwrap();
    let [jit, aot] = both(&name, &["-bfpp"], b"", "bfpp-missing");
    assert_eq!(jit.stdout, vec![0xFF]);
    assert_eq!(aot.stdout, vec![0xFF]);
    let _ = std::fs::remove_file(&name);
}

#[test]
fn brainfuck_pp_commands_are_comments_in_brainfuck() {
    let name = tmp("bf-comments.b");
    std::fs::write(&name, "++++++++[>++++++++<-]>#:;%^!.").unwrap();
    let [jit, aot] = both(&name, &["-bf", "-O0"], b"", "bf-comments");
    assert_eq!(jit.stdout, vec![64u8]);
    assert_eq!(aot.stdout, vec![64u8]);
    let _ = std::fs::remove_file(&name);
}

#[test]
fn brainfuck_pp_socket() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = listener.try_clone().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut conn, _) = acceptor.accept().unwrap();
            let mut buf = [0u8; 1];
            conn.read_exact(&mut buf).unwrap();
            conn.write_all(&[buf[0] + 1]).unwrap();
        }
    });

    let mut src = push_bytes(b"127.0.0.1", 5);
    src += &">".repeat(14);
    src.push('>');
    src += &"+".repeat((port >> 8) as usize);
    src.push('>');
    src += &"+".repeat((port & 0xFF) as usize);
    src.push('>');
    src += &"<".repeat(17);
    src.push('%');
    src.push('.');
    src += &"+".repeat(b'A' as usize);
    src.push_str("^!.");
    let name = tmp("bfpp-net.b");
    std::fs::write(&name, src).unwrap();
    let [jit, aot] = both(&name, &["-bfpp", "--fuel", "0"], b"", "bfpp-net");
    assert_eq!(jit.stdout, vec![0x00, b'A' + 1], "jit");
    assert_eq!(aot.stdout, vec![0x00, b'A' + 1], "aot");
    server.join().unwrap();
    let _ = std::fs::remove_file(&name);
}

#[test]
fn bounds_check_traps() {
    let cases: &[(&str, &[&str], &[u8])] = &[
        ("<+", &[], b""),
        ("<+>", &[], b""),
        (">>>>+<<<<", &["--tape-size", "4"], b""),
        (">>>>>", &["--tape-size", "4"], b""),
        ("+++++++[>++++++++<-]>.<<", &[], b"8"),
        ("+>+>+>+<<<[>]", &["--tape-size", "4"], b""),
        ("+>+>+>+<<<[<]", &[], b""),
        ("+[>+]", &["--tape-size", "16"], b""),
    ];
    for (i, (code, extra, stdout)) in cases.iter().enumerate() {
        let src = tmp(&format!("oob{i}.b"));
        std::fs::write(&src, code).unwrap();
        for fuel in ["0", "100000"] {
            let mut flags: Vec<&str> = vec!["--bounds-check", "--fuel", fuel];
            flags.extend_from_slice(extra);
            let [jit, aot] = both(&src, &flags, b"", &format!("oob{i}-{fuel}"));
            assert!(!jit.status.success(), "{code}: jit should fail");
            assert!(String::from_utf8_lossy(&jit.stderr).contains("out of bounds"), "{code}: jit stderr");
            assert_eq!(jit.stdout, *stdout, "{code}: jit stdout");
            assert_eq!(aot.status.code(), Some(1), "{code}: aot exit status");
            assert!(String::from_utf8_lossy(&aot.stderr).contains("out of bounds"), "{code}: aot stderr");
            assert_eq!(aot.stdout, *stdout, "{code}: aot stdout");
        }
        let _ = std::fs::remove_file(&src);
    }
}

#[test]
fn bounds_check_allows_valid_programs_at_the_edges() {
    let src = tmp("edge.b");
    std::fs::write(&src, ">>>++++++++[<++++++++>-]>.").unwrap();
    for fuel in ["0", "1000"] {
        let [jit, aot] = both(&src, &["--bounds-check", "--fuel", fuel, "--tape-size", "5"], b"", "edge");
        assert_eq!(jit.stdout, vec![0u8], "jit");
        assert_eq!(aot.stdout, vec![0u8], "aot");
        assert!(jit.status.success() && aot.status.success());
    }
    let _ = std::fs::remove_file(&src);
}

#[test]
fn output_is_flushed_before_blocking_read() {
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;

    let src = tmp("prompt.b");
    std::fs::write(&src, "++++++++[>++++++++<-]>.,.").unwrap();
    let exe = tmp("prompt");
    assert!(gbfc().args(["--fuel", "0"]).arg(&src).arg("-o").arg(&exe).status().unwrap().success());

    let mut jit = gbfc();
    jit.args(["-j", "--fuel", "0"]).arg(&src);
    for mut cmd in [jit, Command::new(&exe)] {
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut b = [0u8; 1];
            let _ = stdout.read_exact(&mut b);
            let _ = tx.send(b[0]);
            let mut rest = Vec::new();
            let _ = stdout.read_to_end(&mut rest);
            let _ = tx.send(rest.first().copied().unwrap_or(0));
        });
        let prompt = rx.recv_timeout(Duration::from_secs(10)).expect("prompt was not flushed before read");
        assert_eq!(prompt, b'@');
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"x").unwrap();
        drop(stdin);
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), b'x');
        assert!(child.wait().unwrap().success());
    }
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn multiply_factors() {
    let cases: &[(&str, &[u8])] = &[
        ("+++++++[->++>+++>+++++>+++++++++<<<<]>.>.>.>.", &[14, 21, 35, 63]),
        ("+++++++[->--->-----<<]>.>.", &[235, 221]),
        ("+++++++[->+++++++<]>.", &[49]),
    ];
    for (i, (code, want)) in cases.iter().enumerate() {
        let src = tmp(&format!("mul{i}.b"));
        std::fs::write(&src, code).unwrap();
        for flags in [&["-O1", "--fuel", "0"][..], &["-O1", "--fuel", "0", "--bounds-check"]] {
            let [jit, aot] = both(&src, flags, b"", &format!("mul{i}"));
            assert_eq!(jit.stdout, *want, "case {i} jit {flags:?}");
            assert_eq!(aot.stdout, *want, "case {i} aot {flags:?}");
        }
        let _ = std::fs::remove_file(&src);
    }
}

#[test]
fn deeply_nested_loops_compile_quickly() {
    let depth = 20000;
    let src = tmp("deep.b");
    std::fs::write(&src, format!("+{}{}", "[".repeat(depth), "]".repeat(depth))).unwrap();
    let exe = tmp("deep");
    let started = std::time::Instant::now();
    let status = gbfc().arg(&src).arg("-o").arg(&exe).status().unwrap();
    assert!(status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(30), "took {:?}", started.elapsed());
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn finished_program_with_many_cells_is_folded() {
    let src = tmp("cells.b");
    std::fs::write(&src, ">+".repeat(70000)).unwrap();
    let out = gbfc().args(["--emit", "ir"]).arg(&src).output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    let _ = std::fs::remove_file(&src);
}
