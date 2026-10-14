use std::ffi::OsString;
use std::process::ExitCode;

use fastjj::{editor, privacy, request};
use jj_cli::cli_util::CliRunner;
use jj_cli::command_error::user_error;

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    // Before config loading, jj initialization, or thread creation. Never run
    // jj if the kernel cannot enforce the process-privacy guarantee.
    privacy::install().map_err(|e| format!("cannot enforce process privacy: {e}"))?;
    jj_cli::description_util::register_builtin_text_editor(editor::edit);

    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|s| s == "--request-stdin") {
        if args.len() != 1 {
            return Err(
                "--request-stdin must be used alone; put jj options in the JSON args array".into(),
            );
        }
        let private = request::read(std::io::stdin().lock())?;
        if let Some(cwd) = private.cwd {
            std::env::set_current_dir(cwd).map_err(|_| "cannot open request working directory")?;
        }
        args = private.args.into_iter().map(OsString::from).collect();
    }

    if args == [OsString::from("privacy")] {
        println!(
            "fastjj {} (jj 0.29.0)\nProcess privacy: enforced by {}\nChild processes and exec: denied\nWorker threads: allowed\nGit transport: in-process libgit2\nPrivate request mode: fastjj --request-stdin",
            env!("CARGO_PKG_VERSION"),
            privacy::enforcement_name()
        );
        return Ok(ExitCode::SUCCESS);
    }

    Ok(CliRunner::init()
        .name("fastjj")
        .about("A process-private Jujutsu client. Use --request-stdin for private JSON arguments; fastjj privacy explains the enforced policy.")
        .version(concat!(env!("CARGO_PKG_VERSION"), " (jj 0.29.0)"))
        .with_args(request::jj_args(args))
        .with_config_overrides(request::PRIVACY_CONFIG)
        .add_dispatch_hook(|ui, command, dispatch| {
            let external = match command.matches().subcommand() {
                Some(("util", m)) => matches!(m.subcommand_name(), Some("exec" | "gc")),
                Some(("config", m)) => m.subcommand_name() == Some("edit"),
                Some(("fix", _)) => true,
                _ => false,
            };
            if external {
                return Err(user_error("this command requires external processes, which fastjj denies")
                    .hinted("Use config set for configuration. Run external tools separately when needed."));
            }
            dispatch(ui, command)
        })
        .run())
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("fastjj: {error}");
            ExitCode::FAILURE
        }
    }
}
