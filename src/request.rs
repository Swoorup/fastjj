//! Read private arguments from a bounded stdin JSON document, never from a child.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::PathBuf;

use serde::Deserialize;

pub const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

pub fn read(reader: impl Read) -> io::Result<Request> {
    let mut data = Vec::new();
    reader.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdin request exceeds 1 MiB",
        ));
    }
    let request: Request = serde_json::from_slice(&data).map_err(|_| {
        // Don't echo the input: requests can contain private paths or messages.
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected JSON with an args string array and optional cwd; unknown fields are rejected",
        )
    })?;
    if request.args.iter().any(|s| s.contains('\0'))
        || request
            .cwd
            .as_ref()
            .is_some_and(|p| p.as_os_str().as_encoded_bytes().contains(&0))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "request contains a NUL byte",
        ));
    }
    Ok(request)
}

/// Apply privacy defaults after user options, before any `--` delimiter.
/// The kernel policy is the security boundary; these settings select useful
/// in-process implementations instead of triggering denied subprocess calls.
pub fn jj_args(args: Vec<OsString>) -> Vec<OsString> {
    let mut result = vec![OsString::from("fastjj")];
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    result.extend_from_slice(&args[..split]);
    result.extend(
        [
            "--config=git.subprocess=false",
            "--config=ui.pager=\":builtin\"",
            "--config=ui.editor=\":builtin\"",
            "--config=ui.diff-editor=\":builtin\"",
            "--config=ui.merge-editor=\":builtin\"",
            "--config=core.fsmonitor=\"none\"",
            "--config=debug.suppress-git2-deprecation-warning=true",
        ]
        .map(OsString::from),
    );
    result.extend_from_slice(&args[split..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_private_requests() {
        let request = read(br#"{"args":["status"],"cwd":"/private/repo"}"#.as_slice()).unwrap();
        assert_eq!(request.args, ["status"]);
        assert_eq!(request.cwd, Some(PathBuf::from("/private/repo")));
        for data in [
            r#"{"args":[1]}"#,
            r#"{"args":[],"unknown":true}"#,
            r#"{"args":["\u0000"]}"#,
            r#"{"args":[],"cwd":"\u0000"}"#,
            "private invalid input",
        ] {
            let error = read(data.as_bytes()).unwrap_err();
            assert!(!error.to_string().contains(data));
        }
    }

    #[test]
    fn bounds_input() {
        assert!(
            read(io::repeat(b' ').take(MAX_REQUEST_BYTES + 1))
                .unwrap_err()
                .to_string()
                .contains("1 MiB")
        );
    }

    #[test]
    fn inserts_options_before_path_delimiter() {
        let args = jj_args(
            ["diff", "--", "--config=git.subprocess=true"]
                .map(OsString::from)
                .to_vec(),
        );
        let delimiter = args.iter().position(|s| s == "--").unwrap();
        assert!(
            args[..delimiter]
                .iter()
                .any(|s| s == "--config=git.subprocess=false")
        );
        assert_eq!(args[delimiter + 1], "--config=git.subprocess=true");
    }
}
