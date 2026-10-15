//! Read bounded private JSON arguments from stdin or an inherited descriptor.

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
    #[serde(default)]
    pub prompt: bool,
}

/// Read a request from the fixed inherited descriptor, preserving stdin.
pub fn read_fd() -> io::Result<Request> {
    use std::os::fd::FromRawFd;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: stat is writable and descriptor 3 is borrowed for validation.
    if unsafe { libc::fstat(3, &mut stat) } < 0
        || !matches!(stat.st_mode & libc::S_IFMT, libc::S_IFIFO | libc::S_IFSOCK)
    {
        return Err(io::Error::other(
            "request descriptor 3 must be an inherited pipe or socket",
        ));
    }
    // SAFETY: descriptor 3 was validated above and is consumed exactly once.
    read(unsafe { std::fs::File::from_raw_fd(3) })
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

/// Useful in-process implementations; the kernel policy remains the boundary.
pub const PRIVACY_CONFIG: &str = r#"
[ui]
pager = ":builtin"
editor = ":builtin"
diff-editor = ":builtin"
merge-editor = ":builtin"
[fsmonitor]
backend = "none"
"#;

/// Hand argv to jj verbatim, adding only its in-memory application name.
pub fn jj_args(args: Vec<OsString>) -> Vec<OsString> {
    let mut result = vec![OsString::from("fjj")];
    result.extend(args);
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
}
