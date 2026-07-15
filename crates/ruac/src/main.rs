//! `ruac` — the Rua compiler CLI.
//!
//! Usage:
//!   ruac build <file.rua> [-o <out.lua>]   compile one file to Lua
//!   ruac check <file.rua>                  parse + report errors, no output
//!
//! With no `-o`, `build` writes alongside the input with a `.lua` extension.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    println!(
        "    ruac build <file.rua> [-o <out.lua>] [-c <.ruarc.toml>] [--library <path>] [--library-mount <name>=<path>]"
    );
    println!(
        "    ruac check <file.rua> [-c <.ruarc.toml>] [--library <path>] [--library-mount <name>=<path>]"
    );
    println!("\nWithout `-c`, ruac searches parent directories for `.ruarc.toml`.");
}

fn build(args: &[String]) -> Result<(), String> {
    let args = parse_command_args(args, "build", true)?;
    let options = load_compile_options(&args)?;
    let lua = ruac::compile_path_with_options(&args.input, &options)
        .map_err(|error| error.to_string())?;
    let out = args
        .output
        .unwrap_or_else(|| args.input.with_extension("lua"));
    std::fs::write(&out, lua).map_err(|e| format!("writing {}: {}", out.display(), e))?;
    println!("compiled {} -> {}", args.input.display(), out.display());
    Ok(())
}

fn check(args: &[String]) -> Result<(), String> {
    let args = parse_command_args(args, "check", false)?;
    let options = load_compile_options(&args)?;
    ruac::compile_path_with_options(&args.input, &options).map_err(|error| error.to_string())?;
    println!("ok: {}", args.input.display());
    Ok(())
}

#[derive(Debug)]
struct CommandOptions {
    input: PathBuf,
    output: Option<PathBuf>,
    config: Option<PathBuf>,
    std_path: Option<PathBuf>,
    library: Vec<PathBuf>,
    library_mounts: BTreeMap<String, PathBuf>,
}

fn parse_command_args(
    args: &[String],
    command: &str,
    allow_output: bool,
) -> Result<CommandOptions, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut config: Option<PathBuf> = None;
    let mut std_path: Option<PathBuf> = None;
    let mut library = Vec::new();
    let mut library_mounts = BTreeMap::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" if allow_output => {
                i += 1;
                let path = args
                    .get(i)
                    .ok_or_else(|| format!("{command}: `-o` requires a path"))?;
                output = Some(PathBuf::from(path));
            }
            "-c" | "--config" => {
                i += 1;
                let path = args
                    .get(i)
                    .ok_or_else(|| format!("{command}: `--config` requires a path"))?;
                config = Some(PathBuf::from(path));
            }
            "--std-path" | "--builtins-dir" => {
                i += 1;
                let d = args.get(i).ok_or("`--std-path` requires a path")?;
                std_path = Some(PathBuf::from(d));
            }
            "--library" => {
                i += 1;
                let path = args.get(i).ok_or("`--library` requires a path")?;
                library.push(PathBuf::from(path));
            }
            "--library-mount" => {
                i += 1;
                let mount = args
                    .get(i)
                    .ok_or("`--library-mount` requires `<name>=<path>`")?;
                let (name, path) = parse_library_mount(mount)?;
                library_mounts.insert(name, path);
            }
            other => {
                if other.starts_with('-') {
                    return Err(format!("{command}: unknown flag `{other}`"));
                }
                if input.is_some() {
                    return Err(format!("{command}: unexpected argument `{other}`"));
                }
                input = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }
    Ok(CommandOptions {
        input: input.ok_or_else(|| format!("{command}: missing <file.rua>"))?,
        output,
        config,
        std_path,
        library,
        library_mounts,
    })
}

fn parse_library_mount(value: &str) -> Result<(String, PathBuf), String> {
    let (name, path) = value
        .split_once('=')
        .ok_or("library mount must use `<name>=<path>`")?;
    validate_module_name(name)?;
    if path.is_empty() {
        return Err(format!("library mount `{name}` has an empty path"));
    }
    Ok((name.to_string(), PathBuf::from(path)))
}

fn validate_module_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    if !valid_start || !chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return Err(format!("invalid library mount module name `{name}`"));
    }
    Ok(())
}

fn load_compile_options(command: &CommandOptions) -> Result<ruac::CompileOptions, String> {
    let config = match &command.config {
        Some(path) => Some(absolute_from_cwd(path.clone())?),
        None => discover_project_config(&command.input)?,
    };
    let mut options = config
        .map(|config| parse_project_config(&config))
        .transpose()?
        .unwrap_or_default();

    if let Some(path) = &command.std_path {
        options.std_path = Some(absolute_from_cwd(path.clone())?);
    }
    for path in &command.library {
        options.library.push(absolute_from_cwd(path.clone())?);
    }
    for (name, path) in &command.library_mounts {
        options
            .library_mounts
            .insert(name.clone(), absolute_from_cwd(path.clone())?);
    }
    Ok(options)
}

fn discover_project_config(input: &Path) -> Result<Option<PathBuf>, String> {
    let input = absolute_from_cwd(input.to_path_buf())?;
    let mut directory = input.parent();
    while let Some(current) = directory {
        let candidate = current.join(rua_project::PROJECT_CONFIG_FILE);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
        directory = current.parent();
    }
    Ok(None)
}

fn parse_project_config(path: &Path) -> Result<ruac::CompileOptions, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let config = rua_project::parse_project_config(&source)
        .map_err(|error| format!("parsing {}: {error}", path.display()))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let resolved = config
        .resolve(base)
        .map_err(|error| format!("resolving {}: {error}", path.display()))?;
    Ok(ruac::CompileOptions {
        std_path: resolved.std_path,
        library: resolved.library,
        library_mounts: resolved.library_mounts,
    })
}

fn absolute_from_cwd(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| format!("reading current directory: {error}"))
    }
}
