# GBFC — General BrainFuck Compiler

A compiler for Brainfuck (`.b`) and Brainfuck++ written in Rust. It generates machine code directly: no LLVM, no assembler, no external crates. It is not an interpreter.

* **AOT**: self-contained static ELF executable (`hello` is about 1 KB), no libc and no linker.
* **JIT**: the same machine code runs inside the compiler process (anonymous mapping, RW then RX, never W+X).
* **Target**: Linux x86_64. The code is structured so that other targets can be added.

## Usage

```
cargo build --release          # produces target/release/gbfc

gbfc prog.b                    # AOT: ./prog (ELF)
gbfc prog.b -o out             # AOT with an explicit output name
gbfc -j prog.b                 # JIT: compile and run immediately
gbfc -bfpp prog.b              # same, reading the source as Brainfuck++
gbfc --emit ir  prog.b         # print the optimized IR
gbfc --emit bin prog.b         # raw machine code (objdump -D -b binary -mi386:x86-64)
```

| Option | Meaning |
|---|---|
| `-bf` / `-bfpp` / `-bf++` | source language, Brainfuck or Brainfuck++ (default `-bf`) |
| `-O0` / `-O1` | optimizer off / on (default `-O1`) |
| `--tape-size N` | tape size in cells (default 1048576) |
| `--eof zero\|unchanged\|minus1` | result of `,` at EOF (default `zero`) |
| `--bounds-check` | trap on any access outside the tape |
| `--fuel N` | compile-time evaluation budget in steps (default 100000000, `0` disables) |
| `--target x86_64-linux` | target (default: host) |
| `-v` | print statistics to stderr |

## Semantics

Cells are 8-bit and wrap modulo 256. Every byte other than `+-<>[].,` is a comment. Unbalanced brackets are reported as `line:column`. Tape bounds are not checked unless `--bounds-check` is given; leaving the tape then ends in SIGSEGV.

The executable exits with 0 on success, 1 when a bounds check fails and 2 on an I/O error (a failed write, or a read error other than EOF), and prints a message to stderr in both failure cases. Interrupted system calls are retried and partial writes are completed.

## Languages

`-bf` selects Brainfuck. `-bfpp` (or `-bf++`) selects Brainfuck++, which adds file and socket commands; the Brainfuck commands behave exactly the same, and in Brainfuck mode the extra characters are comments.

| Command | Meaning |
|---|---|
| `#` | no file open: the cell holds the distance to a 0-terminated path, that file is opened read/write (created when missing) and the cell becomes 0x00 on success or 0xFF on failure. File already open: closes it |
| `:` | reads one byte from the file into the current cell, 0x00 at EOF |
| `;` | writes the current cell to the file |
| `%` | no connection open: the cell holds the distance to a 0-terminated dotted IPv4 address followed by a two-byte big-endian port; a TCP connection is opened and the cell becomes 0x00 on success or 0xFF on failure. Connection open: closes it |
| `^` | writes the current cell to the connection |
| `!` | reads one byte from the connection into the current cell, 0x00 when the peer closed it |

At most one file and one connection are open at a time, reads start at the beginning of a freshly opened file, and file and socket errors set the exit status to 2 like any other I/O error.

## Optimizations

`-O1` runs the passes below, in order; `-O0` emits one instruction per source command and none of them apply.

1. **Compile-time execution** — the program prefix runs inside the compiler on a zeroed tape for up to `--fuel` steps: output becomes a constant `Write`, the tape becomes constant stores, the remaining program is kept as the run-time part.
2. **Run-length folding** — runs of the same command (`+++`, `>>>`) collapse into a single operation, including runs separated by pointer moves.
3. **Deferred pointer moves** — moves are not emitted immediately; they are folded into cell offsets, so `>+<` is one add and chains of moves collapse into one.
4. **Constant propagation with dead code elimination** — the compiler tracks which cells hold known values: a loop over a known-zero cell is removed, a conditionally executed block with a known-true guard is inlined, redundant stores disappear, and cells written inside a loop lose their known state.
5. **Zero loops to `set`** — `[-]`, `[+]` and loops that only move a value to a cell with a factor of one become a plain store.
6. **Copy and multiply loops to `muladd`** — `[->+<]`, `[->++>---<<]` and longer ones become one load of the source cell plus a chain of scaled adds, wrapped in a single `If` guard, with the factor derived from the loop step modulo 256.
7. **Scan loops to a 16-byte SSE2 search** — `[>]` and `[<]` become one op that compares sixteen cells per iteration with `pcmpeqb` and locates the zero byte with `bsf`/`bsr`.

The back end adds a number of smaller instruction-level refinements on top of these (reuse of arithmetic flags across a loop condition, one source load per multiply group, the shortest jump and addressing forms, buffered I/O, bounds checks only where the pointer can have moved); they are not itemized here.

## Compile-time evaluation

At `-O1` the compiler runs the program on a zeroed tape before generating code, for at most `--fuel` steps. It stops at the first `,` (input is unknown), when the budget is spent, or when the program touches memory outside the tape. Everything computed up to that point is folded into the executable: the output becomes a constant `Write` operation, the tape contents become constant stores, and the rest of the program runs at run time. The executable behaves the same for any stdin.

* Compile time grows with the budget at roughly 2 ns per step: the default of 100,000,000 steps costs up to about 0.2 s, `--fuel 4000000000` up to about 8 s.
* A program that never terminates, or runs longer than the budget, consumes the whole budget on every compilation.
* Constant output is embedded in the binary up to 1 MiB; anything beyond that is produced at run time.
* A program that reads input gains nothing past its first `,`.
* If stopping inside nested loops would make the program more than twice as large, the pass is skipped.
* `--fuel 0` or `-O0` turns the pass off.

## Design

```
.b -> parser -> IR -> opt -> IR -> backend/<arch> -> CodeImage -+-> format/<os> -> ELF   (AOT)
                                                                +-> jit                  (JIT)
```

| Module | Role |
|---|---|
| `ir.rs` | IR: `Add/Set/MulAdd/Move/In/Out/Write/Loop/If/Scan` plus the Brainfuck++ `FOpen/FClose/FIn/FOut/NetOpen/NetClose/NetIn/NetOut`; every cell operation carries an offset from the data pointer; `Write` is constant output produced by compile-time evaluation; `Scan` is a unit-stride search for a zero cell (`[>]`, `[<]`) |
| `parser.rs` | byte-level parse into IR; `Lang::{Bf, Bfpp}` selects the command set |
| `opt.rs` | run-length folding, pointer moves deferred across loops and `if`s, constant propagation with dead code removal, `[-]` to `set`, copy/multiply loops to `muladd`, scan loops to `scan` |
| `peval.rs` | compile-time evaluation of the program prefix |
| `target.rs` | `Arch` and `Os`, system call numbers (`Syscalls`) |
| `backend/mod.rs` | `CodeGen` trait, `CodegenOptions`, `EntryKind::{Function, Process}` |
| `backend/x86_64.rs` | hand-encoded x86-64; `rbx` holds the data pointer, I/O goes through raw syscalls; loop conditions reuse arithmetic flags, multiply groups share one source load, `scan` searches 16 bytes at a time with SSE2, file and socket commands keep their descriptor in the stack frame |
| `format/` | executable writers per OS (currently `elf.rs`) |
| `jit.rs` | loads and calls `extern "C" fn(*mut u8, usize) -> u32` |

To add a target such as aarch64-linux: add an `Arch` variant, its syscall numbers in `Target::syscalls`, a `backend/aarch64.rs` implementing `CodeGen`, and the `e_machine` value in `format/elf.rs`. A new OS needs an executable writer in `format/` (Mach-O, PE). The IR and the optimizer stay unchanged.

## Benchmarks

Run `bash bench/run.sh` from the repository root; `bench/fetch.sh` downloads the third-party programs it needs and the rest are generated into a temporary directory. `RUNS`, `FUEL` and `TIMEOUT` are environment variables, and a baseline compiler can be compared with `BASELINE=/path/to/gbfc`.

The table below is the output of that script on the machine this compiler was developed on: best of 3 runs, and the exact numbers will differ on other hardware.

| program | -O0 | -O1 --fuel 0 | -O1 --fuel 4000000000 | compile | output |
|---|---:|---:|---:|---:|:---:|
| mandelbrot | 1.29 s | 1.13 s | 2.9 ms | 13.59 s | ✓ |
| mandel (kostya) | 1.16 s | 1.02 s | 2.4 ms | 13.73 s | ✓ |
| long | 4.35 s | 227.0 ms | 2.7 ms | 3.79 s | ✓ |
| hanoi | 5.01 s | 52.0 ms | 2.7 ms | 516.9 ms | ✓ |
| bench (kostya) | 670.7 ms | 68.4 ms | 3.1 ms | 599.9 ms | ✓ |
| print 4M bytes | 16.2 ms | 15.4 ms | 11.0 ms | 28.6 ms | ✓ |
| factor 179424691 | 702.8 ms | 234.4 ms | 234.5 ms | 5.9 ms | ✓ |
| cat 8 MiB | 33.6 ms | 34.8 ms | 34.9 ms | 5.7 ms | ✓ |

`-O1 --fuel 4000000000` moves the whole computation into the compiler (13.6 s for mandelbrot, ~13 s for both mandelbrot variants) and leaves a run time of a few milliseconds; nothing helps programs that stop at the first `,` or are bound by system calls (`factor`, `cat`).

## Tests

```
cargo test --release
```

* every `examples/*.b` runs through AOT and JIT in each optimization mode (`.in` is stdin, `.out` is the expected stdout);
* Brainfuck++ file and socket commands are exercised over a real file and a TCP loopback connection;
