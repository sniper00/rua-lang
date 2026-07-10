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
    println!("    ruac build <file.rua> [-o <out.lua>]");
    println!("    ruac check <file.rua>");
}

fn build(args: &[String]) -> Result<(), String> {
    let (input, out) = parse_build_args(args)?;
    let lua = ruac::compile_path(&input)?;
    let out = out.unwrap_or_else(|| input.with_extension("lua"));
    std::fs::write(&out, lua).map_err(|e| format!("writing {}: {}", out.display(), e))?;
    println!("compiled {} -> {}", input.display(), out.display());
    Ok(())
}

fn check(args: &[String]) -> Result<(), String> {
    let input = args
        .first()
        .map(PathBuf::from)
        .ok_or("check: missing <file.rua>")?;
    let (program, files) = ruac::parse_and_resolve(&input)?;
    ruac::check::check(&program, &files)?;
    ruac::typeck::check(&program, &files)?;
    println!("ok: {}", input.display());
    Ok(())
}

fn parse_build_args(args: &[String]) -> Result<(PathBuf, Option<PathBuf>), String> {
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                let o = args.get(i).ok_or("build: `-o` requires a path")?;
                out = Some(PathBuf::from(o));
            }
            other => {
                if input.is_some() {
                    return Err(format!("build: unexpected argument `{}`", other));
                }
                input = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }
    let input = input.ok_or("build: missing <file.rua>")?;
    Ok((input, out))
}
