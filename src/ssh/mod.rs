//! A libgit2 smart transport backed by native SSH and thread-only jump tunnels.
mod config;
mod tunnel;

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use sha2::{Digest, Sha256};
use ssh2::{CheckResult, KnownHostFileKind, Session};
use zeroize::Zeroizing;

use config::{Endpoint, Options};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;
type PasswordCache = Arc<Mutex<HashMap<PathBuf, Zeroizing<String>>>>;

/// Clear cached credentials when this CLI invocation ends. libgit2 keeps its
/// registered factory alive globally, so relying on that factory's Drop would
/// retain cached values until process termination.
pub struct InvocationGuard {
    passwords: PasswordCache,
}
impl Drop for InvocationGuard {
    fn drop(&mut self) {
        self.passwords
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

/// Register before creating transports or starting worker threads.
///
/// # Safety
/// No other thread may create or register a libgit2 transport during this call.
pub unsafe fn install() -> std::result::Result<InvocationGuard, git2::Error> {
    let passwords = PasswordCache::default();
    for scheme in ["ssh", "git+ssh", "ssh+git"] {
        let passwords = passwords.clone();
        // SAFETY: main installs these once before any jj work or threads.
        unsafe {
            git2::transport::register(scheme, move |remote| {
                Transport::smart(
                    remote,
                    false,
                    NativeSsh {
                        passwords: passwords.clone(),
                    },
                )
            })?;
        }
    }
    Ok(InvocationGuard { passwords })
}

struct NativeSsh {
    passwords: PasswordCache,
}

impl SmartSubtransport for NativeSsh {
    fn action(
        &self,
        url: &str,
        action: Service,
    ) -> std::result::Result<Box<dyn SmartSubtransportStream>, git2::Error> {
        self.connect(url, action)
            .map(|stream| Box::new(stream) as Box<dyn SmartSubtransportStream>)
            .map_err(|error| {
                git2::Error::new(
                    git2::ErrorCode::GenericError,
                    git2::ErrorClass::Ssh,
                    error.to_string(),
                )
            })
    }
    fn close(&self) -> std::result::Result<(), git2::Error> {
        Ok(())
    }
}

impl NativeSsh {
    fn connect(&self, url: &str, action: Service) -> Result<Stream> {
        let endpoint = Endpoint::parse(url)?;
        if endpoint.path.is_empty() || endpoint.path.contains('\0') {
            return Err("invalid SSH repository path".into());
        }
        let options = Options::load(&endpoint)?;
        let mut route = Vec::new();
        build_route(options, &mut route, &mut Vec::new())?;
        let mut previous: Option<Session> = None;
        let mut tunnels = Vec::new();
        for options in route {
            let mut session = Session::new()?;
            if let Some(jump) = previous.take() {
                let (socket, bridge) = tunnel::connect(jump, &options.host, options.port)?;
                tunnels.push(bridge);
                session.set_tcp_stream(socket);
            } else {
                let addresses = (options.host.as_str(), options.port).to_socket_addrs()?;
                let mut connected = None;
                for address in addresses {
                    if let Ok(socket) = TcpStream::connect_timeout(&address, options.timeout) {
                        connected = Some(socket);
                        break;
                    }
                }
                let socket = connected.ok_or("cannot connect to SSH host")?;
                socket.set_nodelay(true)?;
                session.set_tcp_stream(socket);
            }
            session.set_timeout(options.timeout.as_millis().min(u32::MAX as u128) as u32);
            session.set_compress(
                options
                    .values
                    .get("compression")
                    .is_some_and(|v| v[0].eq_ignore_ascii_case("yes")),
            );
            for (key, methods) in [
                ("kexalgorithms", vec![ssh2::MethodType::Kex]),
                ("hostkeyalgorithms", vec![ssh2::MethodType::HostKey]),
                ("pubkeyacceptedalgorithms", vec![ssh2::MethodType::SignAlgo]),
                (
                    "ciphers",
                    vec![ssh2::MethodType::CryptCs, ssh2::MethodType::CryptSc],
                ),
                (
                    "macs",
                    vec![ssh2::MethodType::MacCs, ssh2::MethodType::MacSc],
                ),
            ] {
                if let Some(value) = options.values.get(key) {
                    for method in methods {
                        session.method_pref(
                            method,
                            &algorithm_preferences(&session, method, &value[0])?,
                        )?;
                    }
                }
            }
            if !options.values.contains_key("hostkeyalgorithms") {
                prefer_known_host_algorithms(&session, &options)?;
            }
            session.handshake()?;
            verify_host(&session, &options)?;
            authenticate(&session, &options, &self.passwords)?;
            previous = Some(session);
        }
        let session = previous.ok_or("empty SSH connection route")?;
        let mut channel = session.channel_session()?;
        channel.handle_extended_data(ssh2::ExtendedData::Ignore)?;
        let program = match action {
            Service::UploadPackLs | Service::UploadPack => "git-upload-pack",
            Service::ReceivePackLs | Service::ReceivePack => "git-receive-pack",
        };
        // The remote server executes Git. This string never enters local argv.
        channel.exec(&format!("{program} {}", quote_remote_path(&endpoint.path)))?;
        Ok(Stream {
            channel,
            _tunnels: tunnels,
        })
    }
}

fn build_route(options: Options, route: &mut Vec<Options>, stack: &mut Vec<String>) -> Result<()> {
    let name = format!("{}:{}", options.host, options.port);
    if stack.len() >= 8 || stack.contains(&name) {
        return Err("SSH ProxyJump cycle or nesting limit exceeded".into());
    }
    stack.push(name);
    // Each explicit hop gets its own host configuration and host-key check.
    for jump in &options.jumps {
        let jump = if jump.contains("://") {
            Endpoint::parse(jump)?
        } else {
            Endpoint::parse(&format!("ssh://{jump}/"))?
        };
        let mut jump_options = Options::load(&jump)?;
        // An explicit list supplies the route; don't duplicate a hop's own
        // route when it is already reached through a preceding listed hop.
        if !route.is_empty() {
            jump_options.jumps.clear();
        }
        build_route(jump_options, route, stack)?;
    }
    stack.pop();
    route.push(options);
    Ok(())
}

fn quote_remote_path(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn algorithm_preferences(
    session: &Session,
    method: ssh2::MethodType,
    value: &str,
) -> Result<String> {
    // libssh2 accepts signing preferences but doesn't expose a built-in
    // SignAlgo list; its host-key algorithms describe the same crypto backend.
    let supported = session.supported_algs(if matches!(method, ssh2::MethodType::SignAlgo) {
        ssh2::MethodType::HostKey
    } else {
        method
    })?;
    let preferences = if let Some(add) = value.strip_prefix('+') {
        supported
            .iter()
            .copied()
            .chain(add.split(','))
            .collect::<Vec<_>>()
    } else if let Some(first) = value.strip_prefix('^') {
        first
            .split(',')
            .chain(supported.iter().copied())
            .collect::<Vec<_>>()
    } else if let Some(remove) = value.strip_prefix('-') {
        let patterns = remove
            .split(',')
            .map(glob::Pattern::new)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        supported
            .iter()
            .copied()
            .filter(|algorithm| !patterns.iter().any(|pattern| pattern.matches(algorithm)))
            .collect()
    } else {
        value.split(',').collect()
    };
    let mut result = Vec::new();
    for algorithm in preferences {
        if supported.contains(&algorithm) && !result.contains(&algorithm) {
            result.push(algorithm);
        }
    }
    if result.is_empty() {
        return Err("configured SSH algorithm list has no supported algorithms".into());
    }
    Ok(result.join(","))
}

fn prefer_known_host_algorithms(session: &Session, options: &Options) -> Result<()> {
    let mut preferred = Vec::new();
    for file in &options.known_hosts {
        let contents = match fs::read_to_string(file) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for line in contents
            .lines()
            .filter(|line| !line.trim_start().starts_with(['#', '@']))
        {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 3 {
                continue;
            }
            let Ok(key) = STANDARD.decode(fields[2]) else {
                continue;
            };
            let mut entry = session.known_hosts()?;
            entry.read_str(line, KnownHostFileKind::OpenSSH)?;
            if matches!(
                entry.check_port(&options.host_key_alias, options.port, &key),
                CheckResult::Match
            ) {
                match fields[1] {
                    "ssh-rsa" => {
                        preferred.extend(["rsa-sha2-512".to_owned(), "rsa-sha2-256".to_owned()])
                    }
                    algorithm => preferred.push(algorithm.to_owned()),
                }
            }
        }
    }
    if !preferred.is_empty() {
        session.method_pref(
            ssh2::MethodType::HostKey,
            &algorithm_preferences(
                session,
                ssh2::MethodType::HostKey,
                &format!("^{}", preferred.join(",")),
            )?,
        )?;
    }
    Ok(())
}

fn verify_host(session: &Session, options: &Options) -> Result<()> {
    let (key, kind) = session
        .host_key()
        .ok_or("SSH server supplied no host key")?;
    let mut known = session.known_hosts()?;
    for file in &options.known_hosts {
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for line in text
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        {
            if let Some(revoked) = line
                .trim()
                .strip_prefix("@revoked")
                .filter(|rest| rest.starts_with(char::is_whitespace))
            {
                let mut revoked_hosts = session.known_hosts()?;
                revoked_hosts.read_str(revoked.trim_start(), KnownHostFileKind::OpenSSH)?;
                if matches!(
                    revoked_hosts.check_port(&options.host_key_alias, options.port, key),
                    CheckResult::Match
                ) {
                    return Err("SSH host key is revoked".into());
                }
            } else if !line.trim().starts_with('@') {
                known.read_str(line, KnownHostFileKind::OpenSSH)?;
            }
        }
    }
    match known.check_port(&options.host_key_alias, options.port, key) {
        CheckResult::Match => return Ok(()),
        CheckResult::Mismatch => return Err("SSH host key changed; refusing connection".into()),
        CheckResult::Failure => return Err("cannot verify SSH host key".into()),
        CheckResult::NotFound => {}
    }
    if options.strict == "yes" || options.batch && options.strict != "accept-new" {
        return Err(
            "unknown SSH host key; add a verified key to the configured known_hosts file".into(),
        );
    }
    if options.strict == "ask" {
        let fingerprint = STANDARD
            .encode(Sha256::digest(key))
            .trim_end_matches('=')
            .to_owned();
        let answer = crate::prompt::read(&format!(
            "Trust SSH host {}:{} with fingerprint SHA256:{fingerprint}? Type yes: ",
            options.host_key_alias, options.port
        ), false).map_err(|_| "unknown SSH host key; verify and add it to known_hosts or use an interactive terminal")?;
        if answer != "yes" {
            return Err("SSH host key was not accepted".into());
        }
    }
    let host = if options.port == 22 {
        options.host_key_alias.clone()
    } else {
        format!("[{}]:{}", options.host_key_alias, options.port)
    };
    let format: ssh2::KnownHostKeyFormat = kind.into();
    known.add(&host, key, "fastjj", format)?;
    let entry = known
        .hosts()?
        .into_iter()
        .find(|entry| entry.name() == Some(host.as_str()))
        .ok_or("cannot serialize SSH host key")?;
    let line = known.write_string(&entry, KnownHostFileKind::OpenSSH)?;
    let file = options
        .known_hosts
        .first()
        .ok_or("no known_hosts file configured for storing an accepted key")?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(file)?;
    // A leading newline also handles an existing file without a final newline.
    writeln!(output, "\n{}", line.trim_end())?;
    Ok(())
}

fn public_key(path: &Path) -> Option<Vec<u8>> {
    let public = fs::read_to_string(format!("{}.pub", path.display()))
        .ok()
        .and_then(|text| {
            text.split_whitespace()
                .nth(1)
                .and_then(|blob| STANDARD.decode(blob).ok())
        });
    public.or_else(|| {
        let text = Zeroizing::new(fs::read_to_string(path).ok()?);
        if let Ok(key) = ssh_key::PublicKey::from_openssh(&text) {
            return key.to_bytes().ok();
        }
        ssh_key::PrivateKey::from_openssh(&*text)
            .ok()?
            .public_key()
            .to_bytes()
            .ok()
    })
}

fn authenticate(session: &Session, options: &Options, passwords: &PasswordCache) -> Result<()> {
    let preferred = options
        .values
        .get("preferredauthentications")
        .map(|v| v[0].as_str())
        .unwrap_or("publickey,keyboard-interactive,password");
    let enabled = |key: &str| {
        options
            .values
            .get(key)
            .is_none_or(|v| !v[0].eq_ignore_ascii_case("no"))
    };
    for method in preferred.split(',') {
        // Re-query after partial success, which can change the methods offered
        // by a server requiring more than one authentication factor.
        let offered = session.auth_methods(&options.user)?.to_owned();
        if !offered.split(',').any(|offered| offered == method) {
            continue;
        }
        match method {
            "publickey" if authenticate_keys(session, options, passwords)? => return Ok(()),
            "password" if !options.batch && enabled("passwordauthentication") => {
                let password = Zeroizing::new(
                    crate::prompt::password(format!(
                        "SSH password for {}@{}: ",
                        options.user, options.host
                    ))
                    .map_err(|_| "SSH password authentication requires an interactive terminal")?,
                );
                if session.userauth_password(&options.user, &password).is_ok() {
                    return Ok(());
                }
            }
            "keyboard-interactive"
                if !options.batch
                    && enabled("kbdinteractiveauthentication")
                    && enabled("challengeresponseauthentication") =>
            {
                let mut prompts = NativePrompts { error: false };
                if session
                    .userauth_keyboard_interactive(&options.user, &mut prompts)
                    .is_ok()
                {
                    return Ok(());
                }
                if prompts.error {
                    return Err(
                        "SSH keyboard-interactive authentication requires an interactive terminal"
                            .into(),
                    );
                }
            }
            _ => {}
        }
    }
    Err("SSH authentication failed; configure IdentityFile or an existing IdentityAgent (encrypted files need a terminal or an unlocked agent)".into())
}

struct NativePrompts {
    error: bool,
}
impl ssh2::KeyboardInteractivePrompt for NativePrompts {
    fn prompt<'a>(
        &mut self,
        _username: &str,
        _instructions: &str,
        prompts: &[ssh2::Prompt<'a>],
    ) -> Vec<String> {
        prompts
            .iter()
            .map(|prompt| {
                // Honor server echo hints through the native terminal or private UI channel.
                crate::prompt::read(prompt.text.as_ref(), !prompt.echo).unwrap_or_else(|_| {
                    self.error = true;
                    String::new()
                })
            })
            .collect()
    }
}

fn authenticate_keys(
    session: &Session,
    options: &Options,
    passwords: &PasswordCache,
) -> Result<bool> {
    if options
        .values
        .get("pubkeyauthentication")
        .is_none_or(|v| !v[0].eq_ignore_ascii_case("no"))
    {
        if let Some(socket) = &options.agent {
            let mut agent = session.agent()?;
            agent.set_identity_path(socket)?;
            if agent.connect().is_ok() && agent.list_identities().is_ok() {
                let allowed: Vec<_> = options
                    .identities
                    .iter()
                    .filter_map(|path| public_key(path))
                    .collect();
                for identity in agent.identities()? {
                    if options.identities_only && !allowed.iter().any(|key| key == identity.blob())
                    {
                        continue;
                    }
                    if agent.userauth(&options.user, &identity).is_ok() {
                        return Ok(true);
                    }
                }
            }
        }
        for path in options.identities.iter().filter(|path| path.is_file()) {
            if session
                .userauth_pubkey_file(&options.user, None, path, None)
                .is_ok()
            {
                return Ok(true);
            }
            if let Some(password) = passwords
                .lock()
                .map_err(|_| "SSH password cache unavailable")?
                .get(path)
                && session
                    .userauth_pubkey_file(&options.user, None, path, Some(password))
                    .is_ok()
            {
                return Ok(true);
            }
            let contents = Zeroizing::new(fs::read_to_string(path)?);
            let encrypted = ssh_key::PrivateKey::from_openssh(&*contents)
                .map(|key| key.is_encrypted())
                .unwrap_or_else(|_| contents.contains("ENCRYPTED"));
            if !encrypted {
                continue;
            }
            #[cfg(target_os = "macos")]
            if options.use_keychain
                && let Some(password) = keychain_password(path)
                && session
                    .userauth_pubkey_file(&options.user, None, path, Some(&password))
                    .is_ok()
            {
                return Ok(true);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = options.use_keychain;
            if options.batch {
                continue;
            }
            for _ in 0..3 {
                let password = Zeroizing::new(crate::prompt::password(format!(
                    "Passphrase for SSH key {}: ",
                    path.display()
                )).map_err(|error| format!("SSH key passphrase unavailable: {error}; use a credential UI, terminal, or unlocked agent"))?);
                if session
                    .userauth_pubkey_file(&options.user, None, path, Some(&password))
                    .is_ok()
                {
                    passwords
                        .lock()
                        .map_err(|_| "SSH password cache unavailable")?
                        .insert(path.clone(), password);
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
fn keychain_password(path: &Path) -> Option<Zeroizing<String>> {
    let password = Zeroizing::new(
        security_framework::passwords::get_generic_password("OpenSSH", &path.to_string_lossy())
            .ok()?,
    );
    Some(Zeroizing::new(
        std::str::from_utf8(&password).ok()?.to_owned(),
    ))
}

struct Stream {
    channel: ssh2::Channel,
    _tunnels: Vec<tunnel::Bridge>,
}
impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.channel.read(buf)
    }
}
impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.channel.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.channel.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_paths_are_quoted_without_shell_injection() {
        assert_eq!(quote_remote_path("owner/repo.git"), "'owner/repo.git'");
        assert_eq!(
            quote_remote_path("x'; echo private; '"),
            "'x'\\''; echo private; '\\'''"
        );
    }
}
