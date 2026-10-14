//! Native terminal prompts or a private inherited Unix socket to the UI.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const MAX_PROMPT_BYTES: usize = 128 * 1024;
static CHANNEL: OnceLock<Mutex<UnixStream>> = OnceLock::new();

#[derive(Serialize)]
struct Prompt<'a> {
    prompt: &'a str,
    secret: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    value: Option<String>,
}

/// Takes ownership of the inherited UI channel. Call once before worker threads.
pub fn install(fd: RawFd) -> io::Result<()> {
    let mut kind: libc::c_int = 0;
    let mut kind_length = std::mem::size_of_val(&kind) as libc::socklen_t;
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&address) as libc::socklen_t;
    // SAFETY: address and length point to correctly sized writable storage.
    if unsafe {
        libc::getpeername(
            fd,
            (&mut address as *mut libc::sockaddr_un).cast(),
            &mut length,
        )
    } < 0
        || address.sun_family as i32 != libc::AF_UNIX
        // SAFETY: kind and kind_length point to writable, correctly sized storage.
        || unsafe {
            libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_TYPE,
                (&mut kind as *mut libc::c_int).cast(), &mut kind_length)
        } < 0
        || kind != libc::SOCK_STREAM
    {
        return Err(io::Error::other(
            "UI prompt descriptor must be a connected Unix socket",
        ));
    }
    // SAFETY: getpeername verified the socket; this function takes ownership once.
    let channel = unsafe { UnixStream::from_raw_fd(fd) };
    CHANNEL
        .set(Mutex::new(channel))
        .map_err(|_| io::Error::other("UI prompt channel already initialized"))
}

pub fn available() -> bool {
    CHANNEL.get().is_some()
        || io::stdin().is_terminal()
        || io::stderr().is_terminal()
        || io::stdout().is_terminal()
        || std::env::var("JJ_INTERACTIVE").as_deref() == Ok("1")
}

pub fn read(prompt: &str, secret: bool) -> io::Result<String> {
    if let Some(channel) = CHANNEL.get() {
        let mut channel = channel
            .lock()
            .map_err(|_| io::Error::other("UI prompt channel unavailable"))?;
        return exchange(&mut *channel, prompt, secret);
    }
    if secret {
        return rpassword::prompt_password(prompt);
    }
    let mut terminal = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    write!(terminal, "{prompt}: ")?;
    terminal.flush()?;
    let mut response = String::new();
    if BufReader::new(terminal).read_line(&mut response)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "prompt cancelled",
        ));
    }
    Ok(response.trim_end_matches(['\n', '\r']).to_owned())
}

pub fn password(prompt: impl AsRef<str>) -> io::Result<String> {
    read(prompt.as_ref(), true)
}

fn exchange(channel: &mut (impl Read + Write), prompt: &str, secret: bool) -> io::Result<String> {
    let message = serde_json::to_vec(&Prompt { prompt, secret })?;
    if message.len() > MAX_PROMPT_BYTES {
        return Err(io::Error::other("UI prompt exceeds limit"));
    }
    channel.write_all(&message)?;
    channel.write_all(b"\n")?;
    channel.flush()?;
    // Read exactly one bounded frame, without buffering a subsequent response.
    let mut frame = Zeroizing::new(Vec::new());
    let mut byte = [0];
    loop {
        channel.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        if frame.len() >= MAX_PROMPT_BYTES {
            return Err(io::Error::other("UI response exceeds limit"));
        }
        frame.push(byte[0]);
    }
    let response: Response =
        serde_json::from_slice(&frame).map_err(|_| io::Error::other("invalid UI response"))?;
    response
        .value
        .ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "prompt cancelled"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_or_oversized_prompt_responses_fail_without_echoing_secrets() {
        for response in [
            b"secret invalid JSON\n".to_vec(),
            b"{\"value\":\"secret\",\"extra\":true}\n".to_vec(),
            vec![b's'; MAX_PROMPT_BYTES + 1],
            Vec::new(),
        ] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let worker = std::thread::spawn(move || exchange(&mut client, "prompt", true));
            let mut line = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = server.write_all(&response);
            drop(server);
            let error = worker.join().unwrap().unwrap_err();
            assert!(!error.to_string().contains("secret"));
        }
    }

    #[test]
    fn private_prompt_round_trip_and_cancellation() {
        for response in ["{\"value\":\"secret\\nvalue\"}\n", "{\"value\":null}\n"] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let worker =
                std::thread::spawn(move || exchange(&mut client, "private key path", true));
            let mut line = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                serde_json::json!({"prompt":"private key path", "secret":true})
            );
            server.write_all(response.as_bytes()).unwrap();
            let result = worker.join().unwrap();
            if response.contains("null") {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
            } else {
                assert_eq!(result.unwrap(), "secret\nvalue");
            }
        }
    }
}
