//! Read SSH configuration as data. No directive is evaluated by a shell.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Result;

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub path: String,
}

impl Endpoint {
    pub fn parse(input: &str) -> Result<Self> {
        if input.contains("://") {
            let url = url::Url::parse(input)?;
            if !matches!(url.scheme(), "ssh" | "git+ssh" | "ssh+git")
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("invalid SSH Git URL".into());
            }
            let decode = |value: &str| -> Result<String> {
                Ok(percent_encoding::percent_decode_str(value)
                    .decode_utf8()?
                    .into_owned())
            };
            return Ok(Self {
                host: url
                    .host_str()
                    .ok_or("missing SSH host")?
                    .trim_matches(['[', ']'])
                    .into(),
                user: (!url.username().is_empty())
                    .then(|| decode(url.username()))
                    .transpose()?,
                port: url.port(),
                path: decode(url.path())?,
            });
        }
        let separator = if let Some(end) = input.find(']') {
            end + 1
        } else {
            input.find(':').ok_or("invalid SCP-style Git URL")?
        };
        if input.as_bytes().get(separator) != Some(&b':') {
            return Err("invalid SCP-style Git URL".into());
        }
        let authority = &input[..separator];
        let (user, host) = authority
            .rsplit_once('@')
            .map_or((None, authority), |(u, h)| (Some(u.into()), h));
        Ok(Self {
            host: host.trim_matches(['[', ']']).into(),
            user,
            port: None,
            path: input[separator + 1..].into(),
        })
    }
}

#[derive(Debug)]
pub struct Options {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identities: Vec<PathBuf>,
    pub identities_only: bool,
    pub agent: Option<PathBuf>,
    pub known_hosts: Vec<PathBuf>,
    pub host_key_alias: String,
    pub strict: String,
    pub batch: bool,
    pub timeout: Duration,
    pub jumps: Vec<String>,
    pub use_keychain: bool,
    pub values: HashMap<String, Vec<String>>,
}

impl Options {
    pub fn load(endpoint: &Endpoint) -> Result<Self> {
        let user_dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("missing user home directory")?;
        let files = if let Some(file) = std::env::var_os("FASTJJ_SSH_CONFIG") {
            let file = PathBuf::from(file);
            if !file.is_file() {
                return Err("cannot open FASTJJ_SSH_CONFIG file".into());
            }
            vec![file]
        } else {
            vec![
                user_dir.join(".ssh/config"),
                PathBuf::from("/etc/ssh/ssh_config"),
            ]
        };
        Self::from_files(endpoint, &user_dir, &files)
    }

    fn from_files(endpoint: &Endpoint, user_dir: &Path, files: &[PathBuf]) -> Result<Self> {
        let local_user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "git".into());
        let mut values = HashMap::<String, Vec<String>>::new();
        for file in files {
            if file.exists() {
                parse_file(
                    file,
                    endpoint,
                    &local_user,
                    user_dir,
                    &mut values,
                    &mut true,
                    &mut Vec::new(),
                )?;
            }
        }
        let get = |key: &str| values.get(key).and_then(|v| v.first()).cloned();
        for key in [
            "proxycommand",
            "localcommand",
            "remotecommand",
            "knownhostscommand",
            "securitykeyprovider",
        ] {
            if get(key).is_some_and(|v| !v.eq_ignore_ascii_case("none")) {
                return Err(format!("SSH {key} requires an external program; use native ProxyJump or an existing agent").into());
            }
        }
        if get("certificatefile").is_some() {
            return Err(
                "SSH CertificateFile is not supported by the native libssh2 backend".into(),
            );
        }
        let host = get("hostname")
            .unwrap_or_else(|| endpoint.host.clone())
            .replace("%h", &endpoint.host);
        let user = endpoint
            .user
            .clone()
            .or_else(|| get("user"))
            .unwrap_or(local_user);
        let port = endpoint
            .port
            .or(get("port").map(|s| s.parse()).transpose()?)
            .unwrap_or(22);
        let expand = |s: &str| expand(s, user_dir, &host, &endpoint.host, &user, port);
        let identities = if let Some(files) = values.get("identityfile") {
            files
                .iter()
                .filter(|s| !s.eq_ignore_ascii_case("none"))
                .map(|s| expand(s).map(PathBuf::from))
                .collect::<Result<Vec<_>>>()?
        } else {
            ["id_ed25519", "id_ecdsa", "id_rsa"]
                .into_iter()
                .map(|s| user_dir.join(".ssh").join(s))
                .collect()
        };
        let agent = match get("identityagent") {
            Some(s) if s.eq_ignore_ascii_case("none") => None,
            Some(s) if s == "SSH_AUTH_SOCK" => std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            Some(s) => Some(PathBuf::from(expand(&s)?)),
            None => std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
        };
        let mut known_hosts = Vec::new();
        for (option, defaults) in [
            (
                "userknownhostsfile",
                vec![
                    user_dir.join(".ssh/known_hosts"),
                    user_dir.join(".ssh/known_hosts2"),
                ],
            ),
            (
                "globalknownhostsfile",
                vec![
                    PathBuf::from("/etc/ssh/ssh_known_hosts"),
                    PathBuf::from("/etc/ssh/ssh_known_hosts2"),
                ],
            ),
        ] {
            if let Some(files) = values.get(option) {
                for s in files.iter().filter(|s| !s.eq_ignore_ascii_case("none")) {
                    known_hosts.push(PathBuf::from(expand(s)?));
                }
            } else {
                known_hosts.extend(defaults);
            }
        }
        let strict = get("stricthostkeychecking")
            .unwrap_or_else(|| "ask".into())
            .to_ascii_lowercase();
        if !matches!(strict.as_str(), "yes" | "ask" | "accept-new") {
            return Err(
                "SSH host-key verification cannot be disabled; use yes, ask, or accept-new".into(),
            );
        }
        let boolean = |key: &str| -> Result<bool> {
            match get(key).as_deref().map(str::to_ascii_lowercase).as_deref() {
                None | Some("no") => Ok(false),
                Some("yes") => Ok(true),
                _ => Err(format!("invalid SSH {key} value").into()),
            }
        };
        Ok(Self {
            host_key_alias: get("hostkeyalias").unwrap_or_else(|| host.clone()),
            host,
            user,
            port,
            identities,
            agent,
            known_hosts,
            strict,
            identities_only: boolean("identitiesonly")?,
            batch: boolean("batchmode")?,
            use_keychain: boolean("usekeychain")?,
            timeout: Duration::from_secs(
                get("connecttimeout")
                    .map(|s| s.parse())
                    .transpose()?
                    .unwrap_or(30)
                    .max(1),
            ),
            jumps: get("proxyjump")
                .filter(|s| !s.eq_ignore_ascii_case("none"))
                .map(|s| s.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
            values,
        })
    }
}

fn expand(
    value: &str,
    user_dir: &Path,
    host: &str,
    original: &str,
    user: &str,
    port: u16,
) -> Result<String> {
    let value = if let Some(tail) = value.strip_prefix("~/") {
        format!("{}/{}", user_dir.display(), tail)
    } else {
        value.into()
    };
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => result.push('%'),
            Some('d') => result.push_str(&user_dir.to_string_lossy()),
            Some('h') => result.push_str(host),
            Some('n') => result.push_str(original),
            Some('r') => result.push_str(user),
            Some('p') => result.push_str(&port.to_string()),
            _ => return Err("unsupported SSH path expansion token".into()),
        }
    }
    Ok(result)
}

fn patterns(patterns: &[String], value: &str) -> Result<bool> {
    let mut matched = false;
    for pattern in patterns.iter().flat_map(|p| p.split(',')) {
        let (negative, text) = pattern
            .strip_prefix('!')
            .map_or((false, pattern), |p| (true, p));
        if glob::Pattern::new(&text.to_ascii_lowercase())?.matches(&value.to_ascii_lowercase()) {
            if negative {
                return Ok(false);
            }
            matched = true;
        }
    }
    Ok(matched)
}

fn parse_file(
    file: &Path,
    endpoint: &Endpoint,
    local_user: &str,
    user_dir: &Path,
    values: &mut HashMap<String, Vec<String>>,
    active: &mut bool,
    stack: &mut Vec<PathBuf>,
) -> Result<()> {
    let canonical = file.canonicalize()?;
    if stack.len() >= 16 || stack.contains(&canonical) {
        return Err("SSH Include cycle or nesting limit exceeded".into());
    }
    stack.push(canonical);
    if fs::metadata(file)?.len() > 1_048_576 {
        return Err("SSH configuration is too large".into());
    }
    for line in fs::read_to_string(file)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let split = line
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(line.len());
        let key = line[..split].to_ascii_lowercase();
        let args = shlex::split(
            line[split..]
                .trim_start()
                .trim_start_matches('=')
                .trim_start(),
        )
        .ok_or("invalid SSH configuration quoting")?;
        if args.is_empty() {
            return Err("missing SSH configuration value".into());
        }
        if key == "host" {
            *active = patterns(&args, &endpoint.host)?;
            continue;
        }
        if key == "match" {
            let mut matched = true;
            let mut index = 0;
            while index < args.len() {
                let condition = args[index].to_ascii_lowercase();
                index += 1;
                if condition == "all" {
                    continue;
                }
                if condition == "exec" {
                    return Err(
                        "SSH Match exec requires an external process and is unavailable".into(),
                    );
                }
                let pattern = args.get(index).ok_or("missing SSH Match pattern")?;
                index += 1;
                let user = endpoint
                    .user
                    .as_deref()
                    .or_else(|| {
                        values
                            .get("user")
                            .and_then(|v| v.first())
                            .map(String::as_str)
                    })
                    .unwrap_or(local_user);
                let value = match condition.as_str() {
                    "host" => values
                        .get("hostname")
                        .and_then(|v| v.first())
                        .map(String::as_str)
                        .unwrap_or(&endpoint.host),
                    "originalhost" => &endpoint.host,
                    "user" => user,
                    "localuser" => local_user,
                    _ => return Err(format!("unsupported SSH Match condition: {condition}").into()),
                };
                matched &= patterns(std::slice::from_ref(pattern), value)?;
            }
            *active = matched;
            continue;
        }
        if !*active {
            continue;
        }
        if key == "include" {
            let parent = *active;
            for arg in args {
                let path = if let Some(tail) = arg.strip_prefix("~/") {
                    user_dir.join(tail)
                } else if Path::new(&arg).is_absolute() {
                    PathBuf::from(arg)
                } else {
                    file.parent().unwrap_or(Path::new(".")).join(arg)
                };
                for included in glob::glob(&path.to_string_lossy())? {
                    *active = parent;
                    parse_file(
                        &included?, endpoint, local_user, user_dir, values, active, stack,
                    )?;
                }
            }
            *active = parent;
        } else if key == "identityfile" {
            values.entry(key).or_default().extend(args);
        } else {
            values.entry(key).or_insert(args);
        }
    }
    stack.pop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_resolves_alias_include_negation_and_identity_order() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("keys"),
            "IdentityFile ~/custom-%r\nIdentityFile %d/other\n",
        )
        .unwrap();
        let file = root.path().join("config");
        fs::write(&file, "Host private !excluded\n HostName github.com\n User git\n Port 443\n Include keys\n IdentitiesOnly yes\n IdentityAgent none\n UserKnownHostsFile %d/trusted\nHost *\n User ignored\n IdentityFile ~/fallback\n").unwrap();
        let endpoint = Endpoint::parse("private:owner/repo.git").unwrap();
        let opts =
            Options::from_files(&endpoint, root.path(), std::slice::from_ref(&file)).unwrap();
        assert_eq!(
            (&*opts.host, &*opts.user, opts.port),
            ("github.com", "git", 443)
        );
        assert_eq!(
            opts.identities,
            vec![
                root.path().join("custom-git"),
                root.path().join("other"),
                root.path().join("fallback")
            ]
        );
        assert!(opts.identities_only && opts.agent.is_none());
        let explicit = Endpoint::parse("ssh://other@private:2222/owner/repo%20name.git").unwrap();
        let opts = Options::from_files(&explicit, root.path(), &[file]).unwrap();
        assert_eq!(opts.user, "other");
        assert_eq!(opts.port, 2222);
        assert_eq!(explicit.path, "/owner/repo name.git");
    }
    #[test]
    fn config_rejects_include_cycles_and_external_commands() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("config");
        let endpoint = Endpoint::parse("git@private:path").unwrap();
        for text in [
            "Include config",
            "ProxyCommand echo secret",
            "Match exec echo secret",
            "StrictHostKeyChecking no",
        ] {
            fs::write(&file, text).unwrap();
            assert!(
                Options::from_files(&endpoint, root.path(), std::slice::from_ref(&file)).is_err()
            );
        }
    }
    #[test]
    fn scp_and_ipv6_urls_keep_the_remote_path() {
        for input in [
            "git@[::1]:relative/path",
            "ssh://git@[::1]:443/absolute/path",
        ] {
            let endpoint = Endpoint::parse(input).unwrap();
            assert_eq!(endpoint.host, "::1");
            assert_eq!(endpoint.user.as_deref(), Some("git"));
        }
        assert!(Endpoint::parse("ssh://git:secret@host/path").is_err());
    }
}
