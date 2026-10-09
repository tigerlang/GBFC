# GBFC — General BrainFuck Compiler

A Brainfuck (`.b`) compiler written in Rust. It generates machine code directly: no LLVM, no assembler, no external crates. It is not an interpreter.

* **AOT**: self-contained static ELF executable (`hello` is about 1 KB), no libc and no linker.
* **JIT**: the same machine code runs inside the compiler process (anonymous mapping, RW then RX, never W+X).
* **Target**: Linux x86_64. The code is structured so that other targets can be added.

## Usage

```
cargo build --release          # produces target/release/gbfc

gbfc prog.b                    # AOT: ./prog (ELF)
gbfc prog.b -o out             # AOT with an explicit output name
gbfc -j prog.b                 # JIT: compile and run immediately
gbfc --emit ir  prog.b         # print the optimized IR
gbfc --emit bin prog.b         # raw machine code (objdump -D -b binary -mi386:x86-64)
```

| Option | Meaning |
|---|---|
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
| `ir.rs` | IR: `Add/Set/MulAdd/Move/In/Out/Write/Loop/If/Scan`; every cell operation carries an offset from the data pointer; `Write` is constant output produced by compile-time evaluation; `Scan` is a unit-stride search for a zero cell (`[>]`, `[<]`) |
| `opt.rs` | run-length folding, pointer moves deferred across loops and `if`s, constant propagation with dead code removal, `[-]` to `set`, copy/multiply loops to `muladd`, scan loops to `scan` |
| `peval.rs` | compile-time evaluation of the program prefix |
| `target.rs` | `Arch` and `Os`, system call numbers (`Syscalls`) |
| `backend/mod.rs` | `CodeGen` trait, `CodegenOptions`, `EntryKind::{Function, Process}` |
| `backend/x86_64.rs` | hand-encoded x86-64; `rbx` holds the data pointer, I/O goes through raw syscalls; loop conditions reuse arithmetic flags, multiply groups share one source load, `scan` searches 16 bytes at a time with SSE2 |
| `format/` | executable writers per OS (currently `elf.rs`) |
| `jit.rs` | loads and calls `extern "C" fn(*mut u8, usize) -> u32` |

To add a target such as aarch64-linux: add an `Arch` variant, its syscall numbers in `Target::syscalls`, a `backend/aarch64.rs` implementing `CodeGen`, and the `e_machine` value in `format/elf.rs`. A new OS needs an executable writer in `format/` (Mach-O, PE). The IR and the optimizer stay unchanged.

## Tests

```
cargo test --release
```

* every `examples/*.b` runs through AOT and JIT in each optimization mode (`.in` is stdin, `.out` is the expected stdout);
