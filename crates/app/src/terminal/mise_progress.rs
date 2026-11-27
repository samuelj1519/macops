use super::progress::LiveProgress;
use nix::{
    pty::{Winsize, openpty},
    sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr},
};
use std::{
    fs::File,
    io::{self, Read as _, Write as _},
    process::Stdio,
};

pub(crate) struct MiseProgress {
    master: File,
    renderer: Renderer,
    started: bool,
    display_blocked: bool,
    captured: usize,
}

impl MiseProgress {
    pub(crate) fn open() -> Option<(Self, Stdio, Stdio)> {
        if !super::presentation::interactive() {
            return None;
        }
        let (width, height) = crossterm::terminal::size()
            .ok()
            .filter(|&(width, height)| width > 0 && height > 0)
            .unwrap_or((80, 24));
        let sockets = openpty(
            Some(&Winsize {
                ws_row: height,
                ws_col: width,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .ok()?;
        let master = File::from(sockets.master);
        let slave = File::from(sockets.slave);
        for endpoint in [&master, &slave] {
            rustix::io::fcntl_setfd(endpoint, rustix::io::FdFlags::CLOEXEC).ok()?;
        }
        rustix::fs::fcntl_setfl(&master, rustix::fs::OFlags::NONBLOCK).ok()?;
        let mut attrs = tcgetattr(&slave).ok()?;
        attrs.output_flags.remove(OutputFlags::OPOST);
        tcsetattr(&slave, SetArg::TCSANOW, &attrs).ok()?;
        let second_stream = slave.try_clone().ok()?;
        Some((
            Self {
                master,
                renderer: Renderer {
                    color: super::presentation::color(),
                    ..Renderer::default()
                },
                started: false,
                display_blocked: false,
                captured: 0,
            },
            Stdio::from(slave),
            Stdio::from(second_stream),
        ))
    }

    pub(crate) fn relay(
        &mut self,
        display: &mut LiveProgress,
        capture: &mut File,
        draining: bool,
    ) -> io::Result<()> {
        let mut buffer = [0; 8192];
        for _ in 0..16 {
            let count = match self.master.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(nix::libc::EIO) =>
                {
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            self.captured += count;
            if self.captured > 2 * 1024 * 1024 {
                return Err(io::Error::other("mise progress capture limit"));
            }
            capture.write_all(&buffer[..count])?;
            if !self.display_blocked {
                let rendered = self.renderer.push(&buffer[..count]);
                if !rendered.is_empty() {
                    if !self.started {
                        self.started = display.display(b"\n");
                    }
                    self.display_blocked = !self.started || !display.display(rendered.as_bytes());
                }
            }
            if !draining {
                break;
            }
        }
        Ok(())
    }

    pub(crate) fn finish(&self, display: &mut LiveProgress) {
        if self.started && self.renderer.color {
            let _ = display.display(b"\x1b[0m");
        }
    }
}

#[derive(Default)]
enum Escape {
    #[default]
    Text,
    Start,
    Csi {
        params: String,
        valid: bool,
    },
    String {
        ending: bool,
    },
}

#[derive(Default)]
struct Renderer {
    color: bool,
    state: Escape,
    utf8: Vec<u8>,
    line: usize,
}

impl Renderer {
    fn push(&mut self, input: &[u8]) -> String {
        self.utf8.extend_from_slice(input);
        let mut bytes = std::mem::take(&mut self.utf8);
        if let Err(error) = std::str::from_utf8(&bytes)
            && error.error_len().is_none()
        {
            self.utf8 = bytes.split_off(error.valid_up_to());
        }
        let mut output = String::new();
        for ch in String::from_utf8_lossy(&bytes).chars() {
            self.consume(ch, &mut output);
        }
        output
    }

    fn consume(&mut self, ch: char, output: &mut String) {
        self.state = match std::mem::take(&mut self.state) {
            Escape::Text if ch == '\u{1b}' => Escape::Start,
            Escape::Text => {
                if ch == '\n' {
                    self.line += 1;
                }
                if matches!(ch, '\r' | '\n')
                    || (!ch.is_control()
                        && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
                {
                    output.push(ch);
                }
                Escape::Text
            }
            Escape::Start => match ch {
                '[' => Escape::Csi {
                    params: String::new(),
                    valid: true,
                },
                ']' | 'P' | 'X' | '^' | '_' => Escape::String { ending: false },
                _ => Escape::Text,
            },
            Escape::Csi { params, valid } if ('@'..='~').contains(&ch) => {
                if valid {
                    match (params.as_str(), ch) {
                        ("1", 'A') if self.line > 0 => {
                            output.push_str("\x1b[1A");
                            self.line -= 1;
                        }
                        ("2", 'K') => output.push_str("\x1b[2K"),
                        (_, 'm') if self.color => {
                            output.push_str("\x1b[");
                            output.push_str(&params);
                            output.push('m');
                        }
                        _ => (),
                    }
                }
                Escape::Text
            }
            Escape::Csi { mut params, valid } => {
                let valid = valid && params.len() < 32 && (ch.is_ascii_digit() || ch == ';');
                if valid {
                    params.push(ch);
                }
                Escape::Csi { params, valid }
            }
            Escape::String { ending } if ch == '\u{7}' || (ending && ch == '\\') => Escape::Text,
            Escape::String { .. } => Escape::String {
                ending: ch == '\u{1b}',
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraw_stays_in_owned_rows_and_ignores_other_terminal_commands() {
        let mut renderer = Renderer {
            color: true,
            ..Renderer::default()
        };
        assert_eq!(renderer.push(b"\x1b[1Aone\n\x1b[1"), "one\n");
        assert_eq!(
            renderer.push(b"A\x1b[2Ktwo\x1b]52;abc"),
            "\x1b[1A\x1b[2Ktwo"
        );
        assert_eq!(
            renderer.push(b"xyz\x1b\\\x1b[?25l\x1b[999A\x1b[2J\x1b[32mend"),
            "\x1b[32mend"
        );
    }
}
