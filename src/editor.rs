//! A native, line-oriented description editor using the controlling terminal.
//! It also works when stdin is consumed by a private JSON request.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

static BACKGROUND_REQUEST: AtomicBool = AtomicBool::new(false);

/// Background UI requests have not handed their terminal to the backend.
pub fn set_background_request() {
    BACKGROUND_REQUEST.store(true, Ordering::Relaxed);
}

pub fn edit(path: &Path) -> io::Result<()> {
    if BACKGROUND_REQUEST.load(Ordering::Relaxed) {
        return Err(io::Error::other(
            "text editing requires terminal handoff; use the UI's external editor action, or provide -m/--stdin",
        ));
    }
    if crate::editor_process::enabled() {
        return crate::editor_process::edit(path);
    }
    let original = fs::read_to_string(path)?;
    let mut terminal = OpenOptions::new().read(true).write(true).open("/dev/tty")
        .map_err(|_| io::Error::other("built-in editor requires a controlling terminal; use -m for noninteractive descriptions"))?;
    let mut reader = BufReader::new(terminal.try_clone()?);
    if let Some(replacement) = interact(&mut reader, &mut terminal, &original)? {
        fs::write(path, replacement)?;
    }
    Ok(())
}

fn interact(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    original: &str,
) -> io::Result<Option<String>> {
    writeln!(writer, "\nCurrent text:\n{original}")?;
    writeln!(
        writer,
        "Enter replacement text. .save saves, .keep keeps the original, .cancel aborts."
    )?;
    writeln!(
        writer,
        ".undo removes the last entered line, .clear clears all entered lines; .. escapes a leading dot."
    )?;
    let mut lines = Vec::new();
    loop {
        write!(writer, "> ")?;
        writer.flush()?;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "editor input ended before .save or .keep",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        match line {
            ".save" => {
                return Ok(Some(if lines.is_empty() {
                    String::new()
                } else {
                    lines.join("\n") + "\n"
                }));
            }
            ".keep" => return Ok(None),
            ".cancel" => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "description editing cancelled",
                ));
            }
            ".undo" => {
                lines.pop();
            }
            ".clear" => lines.clear(),
            _ => lines.push(
                line.strip_prefix("..")
                    .map_or(line, |_| &line[1..])
                    .to_owned(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_multiline_unicode_and_escaped_commands() {
        let input = "discard\n.undo\nhello 世界\n..save\n\ntrailer\n.save\n";
        let result = interact(&mut input.as_bytes(), &mut Vec::new(), "original").unwrap();
        assert_eq!(result.as_deref(), Some("hello 世界\n.save\n\ntrailer\n"));
    }

    #[test]
    fn keeps_clears_and_aborts_without_saving() {
        assert!(
            interact(&mut ".keep\n".as_bytes(), &mut Vec::new(), "original")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            interact(
                &mut "line\n.clear\n.save\n".as_bytes(),
                &mut Vec::new(),
                "original"
            )
            .unwrap(),
            Some(String::new())
        );
        assert_eq!(
            interact(&mut ".cancel\n".as_bytes(), &mut Vec::new(), "original")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(
            interact(&mut "incomplete\n".as_bytes(), &mut Vec::new(), "original")
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
