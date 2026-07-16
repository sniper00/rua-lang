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
        "    ruac build <file.rua> [-o <out.lua>] [--emit bundle] [--lua-path <dir>] [-c <.ruarc.toml>] [--library <path>] [--library-mount <name>=<path>]"
    );
    println!(
        "    ruac build <file.rua> --emit modules --out-dir <dir> [--lua-path <dir>] [-c <.ruarc.toml>] [--library <path>] [--library-mount <name>=<path>]"
    );
    println!(
        "    ruac check <file.rua> [-c <.ruarc.toml>] [--library <path>] [--library-mount <name>=<path>]"
    );
    println!("\nWithout `-c`, ruac searches parent directories for `.ruarc.toml`.");
}

fn build(args: &[String]) -> Result<(), String> {
    let args = parse_command_args(args, "build", true)?;
    let options = load_compile_options(&args)?;
    match args.emit {
        EmitMode::Bundle => {
            let lua = ruac::compile_path_with_options(&args.input, &options)
                .map_err(|error| error.to_string())?;
            let out = args
                .output
                .unwrap_or_else(|| args.input.with_extension("lua"));
            std::fs::write(&out, lua).map_err(|e| format!("writing {}: {}", out.display(), e))?;
            println!("compiled {} -> {}", args.input.display(), out.display());
        }
        EmitMode::Modules => {
            let output_dir = args
                .output_dir
                .expect("modules mode validates --out-dir during argument parsing");
            let artifact = ruac::compile_path_modules_artifact_with_options(&args.input, &options)
                .map_err(|error| error.to_string())?;
            for module in &artifact.modules {
                let output = output_dir.join(&module.output_path);
                let parent = output.parent().unwrap_or(&output_dir);
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("creating {}: {error}", parent.display()))?;
                std::fs::write(&output, &module.source)
                    .map_err(|error| format!("writing {}: {error}", output.display()))?;
            }
            println!(
                "compiled {} -> {} ({} Lua modules, entry {})",
                args.input.display(),
                output_dir.display(),
                artifact.modules.len(),
                artifact.root_output_path
            );
        }
    }
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
    output_dir: Option<PathBuf>,
    emit: EmitMode,
    config: Option<PathBuf>,
    std_path: Option<PathBuf>,
    library: Vec<PathBuf>,
    library_mounts: BTreeMap<String, PathBuf>,
    lua_path: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EmitMode {
    #[default]
    Bundle,
    Modules,
}

fn parse_command_args(
    args: &[String],
    command: &str,
    allow_output: bool,
) -> Result<CommandOptions, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut output_dir: Option<PathBuf> = None;
    let mut emit = EmitMode::Bundle;
    let mut config: Option<PathBuf> = None;
    let mut std_path: Option<PathBuf> = None;
    let mut library = Vec::new();
    let mut library_mounts = BTreeMap::new();
    let mut lua_path = Vec::new();
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
            "--out-dir" if allow_output => {
                i += 1;
                let path = args
                    .get(i)
                    .ok_or_else(|| format!("{command}: `--out-dir` requires a path"))?;
                output_dir = Some(PathBuf::from(path));
            }
            "--emit" if allow_output => {
                i += 1;
                emit = match args.get(i).map(String::as_str) {
                    Some("bundle") => EmitMode::Bundle,
                    Some("modules") => EmitMode::Modules,
                    Some(value) => {
                        return Err(format!(
                            "{command}: unknown emit mode `{value}` (expected `bundle` or `modules`)"
                        ));
                    }
                    None => return Err(format!("{command}: `--emit` requires a mode")),
                };
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
            "--lua-path" if allow_output => {
                i += 1;
                let path = args.get(i).ok_or("`--lua-path` requires a directory")?;
                lua_path.push(PathBuf::from(path));
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
    match emit {
        EmitMode::Bundle if output_dir.is_some() => {
            return Err(format!("{command}: `--out-dir` requires `--emit modules`"));
        }
        EmitMode::Modules if output.is_some() => {
            return Err(format!(
                "{command}: `-o`/`--out` cannot be used with `--emit modules`"
            ));
        }
        EmitMode::Modules if output_dir.is_none() => {
            return Err(format!(
                "{command}: `--emit modules` requires `--out-dir <dir>`"
            ));
        }
        EmitMode::Bundle | EmitMode::Modules => {}
    }
    Ok(CommandOptions {
        input: input.ok_or_else(|| format!("{command}: missing <file.rua>"))?,
        output,
        output_dir,
        emit,
        config,
        std_path,
        library,
        library_mounts,
        lua_path,
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
    for path in &command.lua_path {
        options.lua_path.push(absolute_from_cwd(path.clone())?);
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
    for library in &resolved.lua_library {
        if !library.declaration_root.is_dir() {
            return Err(format!(
                "resolving {}: Lua library declaration_root is not a directory: {}",
                path.display(),
                library.declaration_root.display()
            ));
        }
        if !library.runtime_root.is_dir() {
            return Err(format!(
                "resolving {}: Lua library runtime_root is not a directory: {}",
                path.display(),
                library.runtime_root.display()
            ));
        }
    }
    Ok(ruac::CompileOptions {
        std_path: resolved.std_path,
        library: resolved.library,
        library_mounts: resolved.library_mounts,
        lua_path: resolved.lua_path,
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
