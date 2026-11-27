use super::presentation::{self, Tone, fit, paint};
use crate::error::Result;
use std::{
    cell::RefCell,
    io::{IsTerminal, Write},
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    sync::mpsc::{self, RecvTimeoutError, Sender},
    time::Duration,
};

thread_local! {
    static INSTALL_PROGRESS: RefCell<Option<Sender<ProgressEvent>>> = const { RefCell::new(None) };
}

enum ProgressEvent {
    Stage(&'static str),
    Finished(bool),
}

pub(super) fn install_stage(text: &'static str) -> bool {
    INSTALL_PROGRESS.with_borrow(|sender| {
        sender
            .as_ref()
            .is_some_and(|sender| sender.send(ProgressEvent::Stage(text)).is_ok())
    })
}

pub(crate) fn during<T>(label: &'static str, work: impl FnOnce() -> Result<T>) -> Result<T> {
    during_checked(label, work, Result::is_ok)
}

pub(crate) fn during_batch(
    label: &'static str,
    work: impl FnOnce() -> Result<serde_json::Value>,
) -> Result<serde_json::Value> {
    during_checked(label, work, |result| {
        result
            .as_ref()
            .is_ok_and(|value| value["incomplete"] == false)
    })
}

fn during_checked<T>(
    label: &'static str,
    work: impl FnOnce() -> T,
    succeeded: impl FnOnce(&T) -> bool,
) -> T {
    with_progress(
        || {
            install_stage(label);
            work()
        },
        succeeded,
    )
}

pub(crate) fn with_install_progress(
    work: impl FnOnce() -> Result<serde_json::Value>,
) -> Result<serde_json::Value> {
    with_progress(work, |result| {
        result
            .as_ref()
            .is_ok_and(|value| value.get("journal").unwrap_or(value)["phase"] == "succeeded")
    })
}

fn with_progress<T>(work: impl FnOnce() -> T, succeeded: impl FnOnce(&T) -> bool) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            INSTALL_PROGRESS.with_borrow_mut(Option::take);
        }
    }
    if !presentation::interactive()
        || std::env::args_os().any(|arg| arg == "--json" || arg == "--verbose" || arg == "-v")
        || INSTALL_PROGRESS.with_borrow(Option::is_some)
    {
        return work();
    }
    let (sender, stages) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(move || draw_install_progress(&stages));
        INSTALL_PROGRESS.set(Some(sender));
        let _reset = Reset;
        let result = work();
        INSTALL_PROGRESS.with_borrow(|sender| {
            if let Some(sender) = sender {
                let _ = sender.send(ProgressEvent::Finished(succeeded(&result)));
            }
        });
        result
    })
}

fn draw_install_progress(stages: &mpsc::Receiver<ProgressEvent>) {
    let Ok(ProgressEvent::Stage(mut label)) = stages.recv() else {
        return;
    };
    let color = presentation::color();
    let frames = ["◒", "◐", "◓", "◑"];
    let mut frame = 0;
    loop {
        let shown = fit(label, presentation::columns().saturating_sub(6));
        render_progress(frames[frame], Tone::Active, &shown, color, "");
        match stages.recv_timeout(Duration::from_millis(100)) {
            Ok(ProgressEvent::Stage(next)) => {
                render_progress("◇", Tone::Success, &shown, color, "\n");
                label = next;
                frame = 0;
            }
            Err(RecvTimeoutError::Timeout) => frame = (frame + 1) % frames.len(),
            Ok(ProgressEvent::Finished(success)) => {
                let (symbol, tone) = if success {
                    ("◇", Tone::Success)
                } else {
                    ("!", Tone::Warning)
                };
                render_progress(symbol, tone, &shown, color, "\n");
                return;
            }
            Err(RecvTimeoutError::Disconnected) => {
                render_progress("!", Tone::Warning, &shown, color, "\n");
                return;
            }
        }
    }
}

fn render_progress(symbol: &str, tone: Tone, label: &str, color: bool, end: &str) {
    let rail = paint("│", Tone::Muted, color);
    let symbol = paint(symbol, tone, color);
    eprint!("\r\x1b[2K{rail} {symbol}  {label}{end}");
}

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

    pub(super) fn display(&mut self, bytes: &[u8]) -> bool {
        self.flush_pending();
        if self.queued.len() + bytes.len() > 128 * 1024 {
            return false;
        }
        self.queued.extend_from_slice(bytes);
        self.flush_pending();
        true
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
