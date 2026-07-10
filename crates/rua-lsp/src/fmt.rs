//! `rua-fmt` — the Rua source formatter CLI.
//!
//! Usage:
//!   rua-fmt <files...>              format files in-place
//!   rua-fmt --check <files...>      exit 1 if any file differs
//!   rua-fmt --stdout <file>         print formatted file to stdout
//!   rua-fmt                         read stdin, print to stdout
//!   rua-fmt -                       read stdin, print to stdout
//!
//! The formatter preserves comments (B2), blank lines (B3), and wraps long
//! lines (B3). Malformed input is left unchanged per-file, and an error is
//! reported; processing continues for remaining files.
//!
//! Exit codes:
//!   0   all files already formatted (or formatted successfully)
//!   1   formatting needed (--check), or parse/file errors

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rua_syntax::format::{check_format, format_str};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(exit_code) => exit_code,
        Err(e) => {
            eprintln!("rua-fmt: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    // Parse flags and collect positional arguments.
    let mut check_mode = false;
    let mut stdout_mode = false;
    let mut paths: Vec<PathBuf> = Vec::new();

    let mut i = 1; // skip argv[0]
    while i < args.len() {
        match args[i].as_str() {
            "--check" => check_mode = true,
            "--stdout" => stdout_mode = true,
            "-" => paths.push(PathBuf::from("-")),
            f if f.starts_with('-') => {
                return Err(format!("unknown flag `{f}` (try `--check` or `--stdout`)"));
            }
            other => paths.push(PathBuf::from(other)),
        }
        i += 1;
    }

    // Disallow --check + --stdout together.
    if check_mode && stdout_mode {
        return Err("`--check` and `--stdout` are mutually exclusive".into());
    }

    // Stdin mode: no file arguments, or explicit `-`.
    if paths.is_empty() || (paths.len() == 1 && paths[0] == Path::new("-")) {
        if check_mode {
            return Err("`--check` requires at least one file path".into());
        }
        // If `-` was given explicitly, remove it so we don't try to open it.
        return format_stdin();
    }

    // Filter out `-` when mixed with real paths (unusual but handle gracefully).
    if paths.iter().any(|p| p == Path::new("-")) {
        return Err("cannot mix stdin (`-`) with file paths".into());
    }

    if stdout_mode {
        if paths.len() != 1 {
            return Err("`--stdout` accepts exactly one file".into());
        }
        return format_stdout(&paths[0]);
    }

    if check_mode {
        format_check(&paths)
    } else {
        format_in_place(&paths)
    }
}

// --- stdin ----------------------------------------------------------------

fn format_stdin() -> Result<ExitCode, String> {
    let mut src = String::new();
    std::io::stdin()
        .read_to_string(&mut src)
        .map_err(|e| format!("reading stdin: {e}"))?;
    print!("{out}", out = format_str(&src));
    Ok(ExitCode::SUCCESS)
}

// --- --stdout -------------------------------------------------------------

fn format_stdout(path: &Path) -> Result<ExitCode, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("{path}: {e}", path = path.display()))?;
    print!("{out}", out = format_str(&src));
    Ok(ExitCode::SUCCESS)
}

// --- --check --------------------------------------------------------------

fn format_check(paths: &[PathBuf]) -> Result<ExitCode, String> {
    let mut errors = 0usize;
    let mut diffs: Vec<&Path> = Vec::new();

    for path in paths {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rua-fmt: {path}: {e}", path = path.display());
                errors += 1;
                continue;
            }
        };
        if !check_format(&src) {
            eprintln!("{path}", path = path.display());
            diffs.push(path);
        }
    }

    let total = diffs.len() + errors;
    if total > 0 {
        if !diffs.is_empty() {
            eprintln!("rua-fmt: {} file(s) need formatting", diffs.len());
        }
        if errors > 0 {
            eprintln!("rua-fmt: {} file(s) could not be read", errors);
        }
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

// --- in-place -------------------------------------------------------------

fn format_in_place(paths: &[PathBuf]) -> Result<ExitCode, String> {
    let mut errors = 0usize;
    let mut changed = 0usize;

    for path in paths {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rua-fmt: {path}: {e}", path = path.display());
                errors += 1;
                continue;
            }
        };
        let out = format_str(&src);
        if out == src {
            continue; // already formatted
        }
        // Atomic write: temp file in the same directory, then rename.
        if let Err(e) = atomic_write(path, &out) {
            eprintln!("rua-fmt: {path}: {e}", path = path.display());
            errors += 1;
            continue;
        }
        eprintln!("formatted {path}", path = path.display());
        changed += 1;
    }

    if errors > 0 {
        eprintln!("rua-fmt: {} file(s) formatted, {} error(s)", changed, errors);
        Ok(ExitCode::FAILURE)
    } else {
        if changed > 0 {
            eprintln!("rua-fmt: {} file(s) formatted", changed);
        }
        Ok(ExitCode::SUCCESS)
    }
}

/// Write `content` to `path` atomically: write to a temp file in the same
/// directory, then rename over the original. This avoids truncating the
/// user's source if the process crashes mid-write.
fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| format!("bad path: {p}", p = path.display()))?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(".rua-fmt-tmp");

    let tmp_path = dir.join(tmp_name);
    std::fs::write(&tmp_path, content)
        .map_err(|e| format!("writing temp file: {e}"))?;
    std::fs::rename(&tmp_path, path)
        .map_err(|e| format!("renaming temp file: {e}"))?;
    Ok(())
}
