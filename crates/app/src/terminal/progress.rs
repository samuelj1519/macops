use std::{
    io::{IsTerminal, Write},
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
};

pub(crate) struct LiveProgress {
    terminal: std::fs::File,
    streams: [ProgressStream; 2],
    queued: Vec<u8>,
    visible: bool,
}

#[derive(Default)]
struct ProgressStream {
    offset: u64,
    remainder: Vec<u8>,
}

impl LiveProgress {
    pub(crate) fn open() -> Option<Self> {
        let stderr = std::io::stderr();
        if !stderr.is_terminal() || std::env::args_os().any(|arg| arg == "--json") {
            return None;
        }
        let path = nix::unistd::ttyname(&stderr).ok()?;
        let terminal = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY | nix::libc::O_NOFOLLOW)
            .open(path)
            .ok()?;
        let inherited = std::fs::File::from(rustix::io::dup(stderr).ok()?);
        if !terminal.is_terminal()
            || terminal.metadata().ok()?.rdev() != inherited.metadata().ok()?.rdev()
        {
            return None;
        }
        Some(Self {
            terminal,
            streams: Default::default(),
            queued: Vec::new(),
            visible: false,
        })
    }

    fn flush_pending(&mut self) {
        while !self.queued.is_empty() {
            match self.terminal.write(&self.queued) {
                Ok(n) if n > 0 => {
                    self.visible = true;
                    self.queued.drain(..n);
                }
                _ => break,
            }
        }
    }

    pub(crate) fn relay(&mut self, files: [&std::fs::File; 2], draining: bool) {
        const LIMIT: u64 = 2 * 1024 * 1024;
        let mut buffer = [0; 8192];
        for (stream, file) in self.streams.iter_mut().zip(files) {
            while stream.offset < LIMIT {
                let max = usize::try_from(LIMIT - stream.offset).unwrap_or(buffer.len());
                let size = buffer.len().min(max);
                let Ok(count) = file.read_at(&mut buffer[..size], stream.offset) else {
                    break;
                };
                if count == 0 {
                    break;
                }
                stream.offset += count as u64;
                stream.remainder.extend_from_slice(&buffer[..count]);
                let mut bytes = std::mem::take(&mut stream.remainder);
                if let Err(error) = std::str::from_utf8(&bytes)
                    && error.error_len().is_none()
                {
                    stream.remainder = bytes.split_off(error.valid_up_to());
                }
                let native = String::from_utf8_lossy(&bytes);
                let visible = native
                    .split('\r')
                    .map(|line| crate::output::sanitize(line, true))
                    .collect::<Vec<_>>()
                    .join("\r");
                if self.queued.len() + visible.len() <= 128 * 1024 {
                    self.queued.extend_from_slice(visible.as_bytes());
                }
                if !draining {
                    break;
                }
            }
        }
        self.flush_pending();
    }
}

impl Drop for LiveProgress {
    fn drop(&mut self) {
        if self.visible {
            let _ = self.terminal.write(b"\n");
        }
    }
}
