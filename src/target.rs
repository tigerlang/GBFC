use crate::error::{err, Result};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub arch: Arch,
    pub os: Os,
}

#[derive(Clone, Copy, Debug)]
pub struct Syscalls {
    pub read: u32,
    pub write: u32,
    pub exit: u32,
    pub eintr: u32,
}

impl Target {
    pub const LINUX_X86_64: Target = Target { arch: Arch::X86_64, os: Os::Linux };
    pub const SUPPORTED: &'static [Target] = &[Target::LINUX_X86_64];

    pub fn host() -> Option<Target> {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Target::LINUX_X86_64)
        } else {
            None
        }
    }

    pub fn parse(s: &str) -> Result<Target> {
        let norm = s
            .to_ascii_lowercase()
            .replace("-unknown", "")
            .replace("-gnu", "")
            .replace("-musl", "");
        for t in Target::SUPPORTED {
            if t.to_string() == norm {
                return Ok(*t);
            }
        }
        let list: Vec<String> = Target::SUPPORTED.iter().map(|t| t.to_string()).collect();
        err(format!("unsupported target '{s}' (supported: {})", list.join(", ")))
    }

    pub fn syscalls(&self) -> Syscalls {
        match (self.arch, self.os) {
            (Arch::X86_64, Os::Linux) => Syscalls { read: 0, write: 1, exit: 60, eintr: 4 },
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let a = match self.arch {
            Arch::X86_64 => "x86_64",
        };
        let o = match self.os {
            Os::Linux => "linux",
        };
        write!(f, "{a}-{o}")
    }
}
