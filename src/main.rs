use std::ffi::OsString;
use std::process::ExitCode;

use fastjj::{editor, editor_process, privacy, prompt, request};
use jj_cli::cli_util::CliRunner;
use jj_cli::command_error::user_error;

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args == [OsString::from("--editor-service")] {
        editor_process::serve()?;
        return Ok(ExitCode::SUCCESS);
    }
    // The explicitly selected editor runs in a text-only service. Repository
    // code and its worker threads remain subject to the mandatory kernel policy.
    let _editor_service = editor_process::start()?;
    // Before config loading, jj initialization, or thread creation. Never run
    // jj if the kernel cannot enforce the process-privacy guarantee.
    privacy::install().map_err(|e| format!("cannot enforce process privacy: {e}"))?;
    // SAFETY: jj and its workers have not been initialized yet.
    let _ssh_transport = unsafe { fastjj::ssh::install()? };
    jj_cli::description_util::register_builtin_text_editor(editor::edit);
    jj_lib::git::register_credential_prompt(prompt::read);
    jj_cli::ui::register_builtin_prompt(prompt::read, prompt::available);

    if args == [OsString::from("--interface-version")] {
        println!("fjj-embed-v1 jj=0.37.0");
        return Ok(ExitCode::SUCCESS);
    }
    if args
        .first()
        .is_some_and(|s| s == "--request-stdin" || s == "--request-fd")
    {
        if args.len() != 1 {
            return Err(
                "private request flags must be used alone; put jj options in the JSON args array"
                    .into(),
            );
        }
        let inherited = args[0] == "--request-fd";
        let private = if inherited {
            request::read_fd()?
        } else {
            request::read(std::io::stdin().lock())?
        };
        if private.prompt {
            if !inherited {
                return Err("UI prompts require --request-fd".into());
            }
            prompt::install(4)?;
            editor::set_background_request();
        }
        if let Some(cwd) = private.cwd {
            std::env::set_current_dir(cwd).map_err(|_| "cannot open request working directory")?;
        }
        args = private.args.into_iter().map(OsString::from).collect();
    }

    // Keep the former diagnostic command as an alias for existing integrations.
    if args == [OsString::from("embed")] || args == [OsString::from("privacy")] {
        println!(
            "fjj {} (jj 0.37.0)\nEmbedded process policy: enforced by {}\nChild processes and exec: denied\nWorker threads: allowed\nGit transport: in-process libgit2\nRequest modes: --request-stdin, --request-fd",
            env!("CARGO_PKG_VERSION"),
            privacy::enforcement_name()
        );
        if editor_process::enabled() {
            println!("Trusted external editor: enabled through a text-only helper");
        }
        return Ok(ExitCode::SUCCESS);
    }
    if args.len() == 2 && args[0] == "--edit-file" {
        editor::edit(std::path::Path::new(&args[1]))?;
        return Ok(ExitCode::SUCCESS);
    }

    Ok(CliRunner::init()
        .name("fjj")
        .about("An embedded Jujutsu client. Use --request-stdin for in-memory JSON arguments; fjj embed explains the enforced process policy.")
        .version(concat!(env!("CARGO_PKG_VERSION"), " (jj 0.37.0)"))
        .with_args(request::jj_args(args))
        .with_config_overrides(request::PRIVACY_CONFIG)
        .add_dispatch_hook(|ui, command, dispatch| {
            let external = match command.matches().subcommand() {
                Some(("util", m)) => matches!(m.subcommand_name(), Some("exec" | "gc")),
                Some(("fix", _)) => true,
                _ => false,
            };
            if external {
                return Err(user_error("this command requires external processes, which fjj denies")
                    .hinted("Use config set for configuration. Run external tools separately when needed."));
            }
            dispatch(ui, command)
        })
        .run().into())
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("fjj: {error}");
            ExitCode::FAILURE
        }
    }
}
