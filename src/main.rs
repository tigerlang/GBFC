use gbfc::backend::{self, CodegenOptions, EntryKind, EofMode};
use gbfc::error::{Error, Result};
use gbfc::target::Target;
use gbfc::{format, ir, jit, opt, parser, peval};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
GBFC 0.1.0 - General BrainFuck Compiler (direct machine code, AOT + JIT)

USAGE:
    gbfc [OPTIONS] <input.b>

OPTIONS:
    -o <file>             Output file (default: input name without extension)
    -j, --jit             JIT-compile and run immediately instead of writing a file
        --emit <kind>     exe (default) | bin (raw machine code) | ir (optimized IR dump)
    -O0 | -O1             Optimization level (default: -O1)
        --tape-size <n>   Tape size in cells (default: 1048576)
        --eof <mode>      Result of ',' at EOF: zero (default) | unchanged | minus1
        --bounds-check    Trap on any access outside the tape (default: unchecked)
        --fuel <n>        Compile-time execution budget in steps (default: 100000000, 0 = off)
        --target <name>   Target (default: host). Supported: x86_64-linux
    -v, --verbose         Print statistics to stderr
    -h, --help            Show this help
    -V, --version         Show version
";

#[derive(PartialEq)]
enum Emit {
    Exe,
    Bin,
    Ir,
}

struct Cli {
    input: String,
    output: Option<String>,
    jit: bool,
    emit: Emit,
    opt: u8,
    tape: u64,
    eof: EofMode,
    bounds_check: bool,
    fuel: u64,
    target: Option<String>,
    verbose: bool,
}

fn e<T: ToString>(x: T) -> Error {
    Error(x.to_string())
}

fn parse_args(args: Vec<String>) -> Result<Option<Cli>> {
    let mut cli = Cli {
        input: String::new(),
        output: None,
        jit: false,
        emit: Emit::Exe,
        opt: 1,
        tape: 1 << 20,
        eof: EofMode::Zero,
        bounds_check: false,
        fuel: 100_000_000,
        target: None,
        verbose: false,
    };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        let mut value = |name: &str| -> Result<String> {
            inline.clone().or_else(|| it.next()).ok_or_else(|| e(format!("{name} requires a value")))
        };
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("gbfc 0.1.0");
                return Ok(None);
            }
            "-o" => cli.output = Some(value("-o")?),
            "-j" | "--jit" => cli.jit = true,
            "-v" | "--verbose" => cli.verbose = true,
            "-O0" => cli.opt = 0,
            "-O1" | "-O2" | "-O3" => cli.opt = 1,
            "--emit" => {
                cli.emit = match value("--emit")?.as_str() {
                    "exe" => Emit::Exe,
                    "bin" => Emit::Bin,
                    "ir" => Emit::Ir,
                    other => return Err(e(format!("unknown --emit kind '{other}'"))),
                }
            }
            "--tape-size" => {
                let v = value("--tape-size")?;
                cli.tape = v
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=(1 << 32)).contains(n))
                    .ok_or_else(|| e(format!("invalid --tape-size '{v}' (1..4294967296)")))?;
            }
            "--eof" => {
                cli.eof = match value("--eof")?.as_str() {
                    "unchanged" => EofMode::Unchanged,
                    "zero" | "0" => EofMode::Zero,
                    "minus1" | "-1" => EofMode::MinusOne,
                    other => return Err(e(format!("unknown --eof mode '{other}'"))),
                }
            }
            "--bounds-check" => cli.bounds_check = true,
            "--fuel" => {
                let v = value("--fuel")?;
                cli.fuel = v.parse::<u64>().map_err(|_| e(format!("invalid --fuel '{v}'")))?;
            }
            "--target" => cli.target = Some(value("--target")?),
            s if s.starts_with('-') && s.len() > 1 => return Err(e(format!("unknown option '{s}' (see --help)"))),
            _ => {
                if !cli.input.is_empty() {
                    return Err(e("only one input file is supported"));
                }
                cli.input = a;
            }
        }
    }
    if cli.input.is_empty() {
        return Err(e("no input file (see --help)"));
    }
    Ok(Some(cli))
}

fn write_file(path: &str, data: &[u8], executable: bool) -> Result<()> {
    std::fs::write(path, data).map_err(|x| e(format!("cannot write '{path}': {x}")))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = executable;
    Ok(())
}

fn default_output(input: &str, ext: &str) -> String {
    let stem = Path::new(input).file_stem().and_then(|s| s.to_str()).unwrap_or("a");
    let name = if ext.is_empty() { stem.to_string() } else { format!("{stem}.{ext}") };
    if name == input {
        "a.out".to_string()
    } else {
        name
    }
}

fn run(cli: Cli) -> Result<()> {
    let src = std::fs::read(&cli.input).map_err(|x| e(format!("cannot read '{}': {x}", cli.input)))?;
    match Path::new(&cli.input).extension().and_then(|s| s.to_str()) {
        Some("b") | Some("bf") => {}
        _ => eprintln!("gbfc: warning: '{}' does not have the canonical .b extension", cli.input),
    }

    let raw = parser::parse(&src).map_err(|x| e(format!("{}:{x}", cli.input)))?;
    let raw_count = ir::count(&raw);
    let ops = if cli.opt >= 1 { opt::optimize(raw) } else { raw };
    if cli.verbose {
        eprintln!("gbfc: {} source bytes, {raw_count} ops -> {} ops (-O{})", src.len(), ir::count(&ops), cli.opt);
    }
    let ops = if cli.opt >= 1 && cli.fuel > 0 {
        let cfg = peval::Config { fuel: cli.fuel, tape_size: cli.tape as usize };
        let t0 = std::time::Instant::now();
        let (ops, st) = peval::evaluate_with_stats(ops, &cfg);
        let ops = opt::fold_constants(&ops);
        if cli.verbose {
            let how = if st.finished { "ran to completion" } else { "stopped, rest runs at run time" };
            eprintln!(
                "gbfc: compile-time execution: {} steps in {:.2?}, {how}, {} bytes of static output -> {} ops",
                st.steps,
                t0.elapsed(),
                st.static_output,
                ir::count(&ops)
            );
        }
        ops
    } else {
        ops
    };

    if cli.emit == Emit::Ir {
        let text = ir::dump(&ops);
        return match &cli.output {
            Some(p) => write_file(p, text.as_bytes(), false),
            None => {
                print!("{text}");
                Ok(())
            }
        };
    }

    let target = match &cli.target {
        Some(t) => Target::parse(t)?,
        None => Target::host().unwrap_or(Target::LINUX_X86_64),
    };
    let cg = backend::for_arch(target.arch);
    let opts = CodegenOptions { eof: cli.eof, bounds_check: cli.bounds_check };
    let sys = target.syscalls();

    if cli.jit || cli.emit == Emit::Bin {
        let image = cg.generate(&ops, &opts, &sys, EntryKind::Function)?;
        if cli.verbose {
            eprintln!("gbfc: {} bytes of {target} machine code", image.bytes.len());
        }
        if cli.jit {
            return jit::run(&target, &image, cli.tape as usize);
        }
        let out = cli.output.clone().unwrap_or_else(|| default_output(&cli.input, "bin"));
        return write_file(&out, &image.bytes, false);
    }

    let layout = format::exe_layout(&target);
    let image = cg.generate(&ops, &opts, &sys, EntryKind::Process { tape_addr: layout.tape_addr, tape_size: cli.tape })?;
    let bytes = format::write_executable(&target, &image, cli.tape)?;
    let out = cli.output.clone().unwrap_or_else(|| default_output(&cli.input, ""));
    write_file(&out, &bytes, true)?;
    if cli.verbose {
        eprintln!("gbfc: {} bytes of {target} machine code, wrote '{out}' ({} bytes)", image.bytes.len(), bytes.len());
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = match parse_args(std::env::args().skip(1).collect()) {
        Ok(Some(c)) => c,
        Ok(None) => return ExitCode::SUCCESS,
        Err(x) => {
            eprintln!("gbfc: error: {x}");
            return ExitCode::FAILURE;
        }
    };
    let handle = std::thread::Builder::new().stack_size(1 << 29).spawn(move || run(cli));
    match handle.map(|h| h.join()) {
        Ok(Ok(Ok(()))) => ExitCode::SUCCESS,
        Ok(Ok(Err(x))) => {
            eprintln!("gbfc: error: {x}");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("gbfc: internal error");
            ExitCode::FAILURE
        }
    }
}
