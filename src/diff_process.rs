//! Trusted Delta rendering with patch contents carried over anonymous pipes.
//!
//! Only user-level jj settings authorize a program and its static options.
//! Repository settings can select an authorized tool, but cannot add commands.

use std::collections::HashMap;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use jj_cli::config::{CommandNameAndArgs, ConfigEnv};
use jj_cli::merge_tools::{ExternalMergeTool, get_external_tool_config};
use jj_lib::config::{ConfigLayer, ConfigSource};
use jj_lib::settings::UserSettings;
use serde::{Deserialize, Serialize};

const MAX_DIFF: usize = 64 * 1024 * 1024;
const MAX_METADATA: usize = 64 * 1024;
static CONNECTION: OnceLock<Arc<Mutex<Connection>>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tool {
    program: String,
    args: Vec<String>,
    expected_codes: Vec<i32>,
}

impl Tool {
    fn from_external(tool: &ExternalMergeTool) -> io::Result<Self> {
        // Delta consumes a Git patch on stdin. Keep filenames in the patch,
        // never in argv, even with an existing $left/$right configuration.
        let args: Vec<_> = tool
            .diff_args
            .iter()
            .filter(|arg| !matches!(arg.as_str(), "$left" | "$right"))
            .cloned()
            .collect();
        if args.iter().any(|arg| {
            jj_cli::config::find_all_variables(std::slice::from_ref(arg))
                .any(|variable| variable != "width")
        }) {
            return Err(io::Error::other(
                "Delta arguments support $width and separate $left/$right arguments only",
            ));
        }
        Ok(Self {
            program: tool.program.clone(),
            args,
            expected_codes: tool.diff_expected_exit_codes.clone(),
        })
    }
}

#[derive(Clone)]
struct TrustedTool {
    command: Tool,
    env: HashMap<String, String>,
}

impl TrustedTool {
    fn new(tool: &ExternalMergeTool, command: Option<&CommandNameAndArgs>) -> io::Result<Self> {
        let env = match command {
            Some(CommandNameAndArgs::Structured { env, .. }) => env.clone(),
            _ => HashMap::new(),
        };
        Ok(Self {
            command: Tool::from_external(tool)?,
            env,
        })
    }
}

struct TrustedConfig {
    tools: Vec<TrustedTool>,
    defaults: ConfigLayer,
    pager: Option<ExternalMergeTool>,
}

impl TrustedConfig {
    fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let environment = ConfigEnv::from_environment();
        let mut config =
            jj_cli::config::config_from_environment(jj_cli::config::default_config_layers());
        environment.reload_user_config(&mut config)?;
        let settings = UserSettings::from_config(environment.resolve_config(&config)?)?;
        let mut tools = Vec::new();
        let mut defaults =
            ConfigLayer::parse(ConfigSource::Default, "aliases.difftool = ['diff']")?;
        let resolve = |args: &CommandNameAndArgs| {
            Ok::<_, jj_lib::config::ConfigGetError>(
                args.as_str()
                    .map(|name| get_external_tool_config(&settings, name))
                    .transpose()?
                    .flatten()
                    .unwrap_or_else(|| ExternalMergeTool::with_diff_args(args)),
            )
        };
        let formatter: CommandNameAndArgs = settings.get("ui.diff-formatter")?;
        let formatter_tool = resolve(&formatter)?;
        if is_delta(&formatter_tool) {
            tools.push(TrustedTool::new(&formatter_tool, Some(&formatter))?);
        }
        for name in settings.table_keys("merge-tools") {
            if let Some(tool) = get_external_tool_config(&settings, name)?
                && is_delta(&tool)
            {
                tools.push(TrustedTool::new(&tool, None)?);
            }
        }
        let pager_command: CommandNameAndArgs = settings.get("ui.pager")?;
        let pager_tool = ExternalMergeTool::with_diff_args(&pager_command);
        let pager = is_delta(&pager_tool).then_some(pager_tool);
        if let Some(pager_tool) = &pager {
            let tool = TrustedTool::new(pager_tool, Some(&pager_command))?;
            // This is a default layer: explicit user/repo/CLI formatter
            // settings and --git/--color-words retain their usual precedence.
            let mut command = vec![tool.command.program.clone()];
            command.extend(tool.command.args.clone());
            defaults = ConfigLayer::parse(
                ConfigSource::Default,
                &format!(
                    "aliases.difftool = ['diff']\nui.diff-formatter = {}",
                    serde_json::to_string(&command)?
                ),
            )?;
            tools.push(tool);
        }
        // Once a user authorizes Delta, --tool delta may use its ordinary
        // options as well as the exact configured argument vectors.
        for tool in tools.clone() {
            let bare = TrustedTool {
                command: Tool {
                    args: vec![],
                    expected_codes: vec![0],
                    ..tool.command
                },
                ..tool
            };
            if !tools.iter().any(|tool| tool.command == bare.command) {
                tools.push(bare);
            }
        }
        Ok(Self {
            tools,
            defaults,
            pager,
        })
    }

    fn authorize(&self, requested: &Tool) -> io::Result<&TrustedTool> {
        self.tools
            .iter()
            .find(|allowed| {
                (requested.program == allowed.command.program || requested.program == "delta")
                    && requested.args == allowed.command.args
                    && requested.expected_codes == allowed.command.expected_codes
            })
            .ok_or_else(|| {
                io::Error::other(
                    "Delta command/options are not authorized by user jj config; configure ui.pager, ui.diff-formatter, or merge-tools in your user config",
                )
            })
    }
}

struct Connection {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

pub struct Service(Arc<Mutex<Connection>>);

impl Drop for Service {
    fn drop(&mut self) {
        let mut connection = self.0.lock().unwrap_or_else(|e| e.into_inner());
        connection.input.take();
        let _ = connection.child.wait();
    }
}

/// Read trusted user configuration while single-threaded, before the repository
/// guard. No repository/workspace config, aliases, or commands are evaluated.
pub fn start() -> Result<(Option<Service>, ConfigLayer), Box<dyn std::error::Error>> {
    let config = TrustedConfig::load()?;
    if let Some(pager) = &config.pager {
        jj_cli::diff_util::register_embedded_diff_pager(pager.clone());
    }
    if config.tools.is_empty() {
        return Ok((None, config.defaults));
    }
    crate::editor_process::protect_ui_descriptors()?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg0("fjj")
        .arg("--diff-service")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let connection = Arc::new(Mutex::new(Connection {
        input: child.stdin.take(),
        output: child.stdout.take().expect("piped diff service stdout"),
        child,
    }));
    CONNECTION
        .set(connection.clone())
        .map_err(|_| io::Error::other("diff service already initialized"))?;
    Ok((Some(Service(connection)), config.defaults))
}

pub fn is_delta(tool: &ExternalMergeTool) -> bool {
    Path::new(&tool.program)
        .file_name()
        .is_some_and(|name| name == "delta")
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    tool: Tool,
    width: usize,
    color: bool,
}

pub fn render(
    tool: &ExternalMergeTool,
    patch: &[u8],
    width: usize,
    color: bool,
) -> io::Result<Vec<u8>> {
    let connection = CONNECTION.get().ok_or_else(|| {
        io::Error::other("Configure Delta in user jj config before using it as a diff tool")
    })?;
    let request = Request {
        tool: Tool::from_external(tool)?,
        width: width.clamp(1, 10000),
        color,
    };
    let mut connection = connection
        .lock()
        .map_err(|_| io::Error::other("diff service failed"))?;
    let input = connection.input.as_mut().expect("live diff service");
    write_frame(input, &serde_json::to_vec(&request)?, MAX_METADATA)?;
    write_frame(input, patch, MAX_DIFF)?;
    let mut status = [0];
    connection.output.read_exact(&mut status)?;
    let response = read_frame(&mut connection.output, MAX_DIFF)?
        .ok_or_else(|| io::Error::other("diff service closed"))?;
    match status[0] {
        0 => Ok(response),
        1 => Err(io::Error::other(
            String::from_utf8_lossy(&response).into_owned(),
        )),
        _ => Err(io::Error::other("invalid diff service response")),
    }
}

pub fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let config = TrustedConfig::load()?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    while let Some(metadata) = read_frame(&mut input, MAX_METADATA)? {
        let patch = read_frame(&mut input, MAX_DIFF)?
            .ok_or_else(|| io::Error::other("missing diff input"))?;
        let result = serde_json::from_slice::<Request>(&metadata)
            .map_err(|_| io::Error::other("invalid diff request"))
            .and_then(|request| {
                let tool = config.authorize(&request.tool)?;
                run_delta(tool, &patch, request.width.clamp(1, 10000), request.color)
            });
        let (status, response) = match result {
            Ok(output) => (0, output),
            Err(error) => (1, error.to_string().into_bytes()),
        };
        output.write_all(&[status])?;
        write_frame(&mut output, &response, MAX_DIFF)?;
    }
    Ok(())
}

fn run_delta(
    allowed: &TrustedTool,
    patch: &[u8],
    width: usize,
    color: bool,
) -> io::Result<Vec<u8>> {
    // Delta's --raw can still emit colors from line-number/side-by-side styles.
    // Preserve a plain, parsable Git patch when jj has disabled color.
    if !color {
        return Ok(patch.to_vec());
    }
    let directory = tempfile::Builder::new()
        .prefix("fjj-diff-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in("/tmp")?;
    let cwd = directory.path().canonicalize()?;
    let input = cwd.join("INPUT");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&input)?
        .write_all(patch)?;
    let tool = &allowed.command;
    let program = Path::new(&tool.program);
    let program = if program.is_relative() && program.components().count() > 1 {
        std::env::current_dir()?.join(program)
    } else {
        program.to_owned()
    };
    let patterns = HashMap::from([("width", width.to_string())]);
    let options = jj_cli::config::interpolate_variables(&tool.args, &patterns);
    // Always capture rendering. Delta must never start its own pager, including
    // when the user's gitconfig or DELTA_PAGER requests one.
    let mut args = Vec::new();
    let mut options = options.into_iter();
    while let Some(arg) = options.next() {
        if arg == "--paging" {
            options.next();
        } else if !arg.starts_with("--paging=") {
            args.push(arg);
        }
    }
    let mut enforced = vec!["--paging=never".to_owned()];
    if !args
        .iter()
        .any(|arg| arg == "-w" || arg == "--width" || arg.starts_with("--width="))
    {
        enforced.push(format!("--width={width}"));
    }
    enforced.extend(args);
    let mut command = Command::new(program);
    // SAFETY: setsid is async-signal-safe and this callback only invokes that
    // syscall. Detaching prevents terminal theme queries from stealing UI input.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .arg0("delta")
        .args(enforced)
        .envs(&allowed.env)
        .current_dir(&cwd)
        .env("PWD", &cwd)
        .env("COLUMNS", width.to_string())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(File::open(input)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            io::Error::other(format!(
                "Cannot run Delta: {error}; install delta or select a built-in diff format"
            ))
        })?;
    let stderr = child.stderr.take().expect("piped Delta stderr");
    let diagnostics = std::thread::spawn(move || read_limited(stderr, 1024 * 1024));
    let result = read_limited(child.stdout.take().expect("piped Delta stdout"), MAX_DIFF);
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let diagnostics = diagnostics
        .join()
        .map_err(|_| io::Error::other("Delta stderr reader failed"))??;
    let result = result?;
    if !status
        .code()
        .is_some_and(|code| tool.expected_codes.contains(&code))
    {
        return Err(io::Error::other(format!(
            "Delta failed ({status}): {}",
            String::from_utf8_lossy(&diagnostics).trim()
        )));
    }
    Ok(result)
}

fn read_limited(reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("diff output exceeds the size limit"));
    }
    Ok(bytes)
}

fn write_frame(writer: &mut impl Write, data: &[u8], limit: usize) -> io::Result<()> {
    if data.len() > limit {
        return Err(io::Error::other("diff input exceeds the size limit"));
    }
    writer.write_all(&(data.len() as u32).to_be_bytes())?;
    writer.write_all(data)?;
    writer.flush()
}

fn read_frame(reader: &mut impl Read, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0; 4];
    loop {
        match reader.read(&mut length[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    reader.read_exact(&mut length[1..])?;
    let length = u32::from_be_bytes(length) as usize;
    if length > limit {
        return Err(io::Error::other("diff frame exceeds the size limit"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_options_cannot_change_the_authorized_program_or_arguments() {
        let tool = Tool {
            program: "/trusted/bin/delta".into(),
            args: vec!["--dark".into()],
            expected_codes: vec![0],
        };
        let config = TrustedConfig {
            tools: vec![TrustedTool {
                command: tool.clone(),
                env: HashMap::new(),
            }],
            defaults: ConfigLayer::empty(ConfigSource::Default),
            pager: None,
        };
        assert_eq!(config.authorize(&tool).unwrap().command, tool);
        let mut requested = tool.clone();
        requested.program = "delta".into();
        assert_eq!(
            config.authorize(&requested).unwrap().command.program,
            tool.program
        );
        requested.program = "/repository/delta".into();
        assert!(config.authorize(&requested).is_err());
        requested = tool.clone();
        requested.args.push("--config=repository/config".into());
        assert!(config.authorize(&requested).is_err());
        requested = tool.clone();
        requested.expected_codes.push(7);
        assert!(config.authorize(&requested).is_err());
    }

    #[test]
    fn protocol_rejects_truncation_unknown_fields_and_oversized_frames() {
        assert!(read_frame(&mut &[][..], MAX_METADATA).unwrap().is_none());
        assert!(read_frame(&mut &[0, 0][..], MAX_METADATA).is_err());
        assert!(read_frame(&mut &[0, 0, 0, 1][..], MAX_METADATA).is_err());
        assert!(
            read_frame(
                &mut (MAX_METADATA as u32 + 1).to_be_bytes().as_slice(),
                MAX_METADATA
            )
            .is_err()
        );
        let invalid = br#"{"tool":{"program":"delta","args":[],"expected_codes":[0]},"width":80,"color":true,"cwd":"/repository"}"#;
        assert!(serde_json::from_slice::<Request>(invalid).is_err());
    }
}
