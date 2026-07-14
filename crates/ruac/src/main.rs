//! `ruac` — the Rua compiler CLI.
//!
//! Usage:
//!   ruac build <file.rua> [-o <out.lua>]   compile one file to Lua
//!   ruac check <file.rua>                  parse + report errors, no output
//!
//! With no `-o`, `build` writes alongside the input with a `.lua` extension.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ruac: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let cmd = args.get(1).map(String::as_str);
    match cmd {
        Some("build") => build(&args[2..]),
        Some("check") => check(&args[2..]),
        Some("-h") | Some("--help") | None => {
            print_usage();
            Ok(())
        }
        Some(other) => Err(format!(
            "unknown command `{}` (try `build`, `check`, or `--help`)",
            other
        )),
    }
}

fn print_usage() {
    println!("ruac — Rua -> Lua 5.5 compiler\n");
    println!("USAGE:");
    println!("    ruac build <file.rua> [-o <out.lua>] [--builtins-dir <dir>]");
    println!("    ruac check <file.rua> [--builtins-dir <dir>]");
}

fn build(args: &[String]) -> Result<(), String> {
    let (input, out, builtins_dir) = parse_build_args(args)?;
    let lua = if let Some(ref d) = builtins_dir {
        ruac::compile_path_with_builtins(&input, d)
    } else {
        ruac::compile_path(&input)
    }
    .map_err(|error| error.to_string())?;
    let out = out.unwrap_or_else(|| input.with_extension("lua"));
    std::fs::write(&out, lua).map_err(|e| format!("writing {}: {}", out.display(), e))?;
    println!("compiled {} -> {}", input.display(), out.display());
    Ok(())
}

fn check(args: &[String]) -> Result<(), String> {
    let (input, builtins_dir) = parse_check_args(args)?;
    if let Some(ref directory) = builtins_dir {
        ruac::compile_path_with_builtins(&input, directory)
    } else {
        ruac::compile_path(&input)
    }
    .map_err(|error| error.to_string())?;
    println!("ok: {}", input.display());
    Ok(())
}

fn parse_build_args(
    args: &[String],
) -> Result<(PathBuf, Option<PathBuf>, Option<PathBuf>), String> {
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut builtins_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                let o = args.get(i).ok_or("build: `-o` requires a path")?;
                out = Some(PathBuf::from(o));
            }
            "--builtins-dir" => {
                i += 1;
                let d = args.get(i).ok_or("`--builtins-dir` requires a path")?;
                builtins_dir = Some(PathBuf::from(d));
            }
            other => {
                if other.starts_with('-') {
                    return Err(format!("build: unknown flag `{}`", other));
                }
                if input.is_some() {
                    return Err(format!("build: unexpected argument `{}`", other));
                }
                input = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }
    let input = input.ok_or("build: missing <file.rua>")?;
    Ok((input, out, builtins_dir))
}

fn parse_check_args(args: &[String]) -> Result<(PathBuf, Option<PathBuf>), String> {
    let mut input: Option<PathBuf> = None;
    let mut builtins_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--builtins-dir" => {
                i += 1;
                let d = args.get(i).ok_or("`--builtins-dir` requires a path")?;
                builtins_dir = Some(PathBuf::from(d));
            }
            other => {
                if other.starts_with('-') {
                    return Err(format!("check: unknown flag `{}`", other));
                }
                if input.is_some() {
                    return Err(format!("check: unexpected argument `{}`", other));
                }
                input = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }
    let input = input.ok_or("check: missing <file.rua>")?;
    Ok((input, builtins_dir))
}
