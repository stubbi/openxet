//! The hook's end: `openxet proc-receive` between git's proc-receive
//! protocol on stdin and stdout (githooks(5)) and the channel to the parent.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use super::{HOOK_FD_ENV, HOOK_TOKEN_ENV, read_section, trim_newline, write_section};

/// The hook's end of the channel, adopted from the descriptors the parent
/// passed down.
pub struct HookChannel {
    reader: BufReader<File>,
    writer: File,
    token: String,
}

impl std::fmt::Debug for HookChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookChannel")
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl HookChannel {
    /// Opens the channel named by `OPENXET_HOOK_FD` and reads
    /// `OPENXET_HOOK_TOKEN`.
    ///
    /// # Errors
    ///
    /// An error naming the variable when either is missing or malformed, or
    /// the descriptors can't be opened.
    pub fn from_env() -> io::Result<Self> {
        let missing = |what: &str| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{what} is not set; `openxet proc-receive` runs only as the hook of a push \
                     that openxet serves"
                ),
            )
        };
        let fds = std::env::var(HOOK_FD_ENV).map_err(|_| missing(HOOK_FD_ENV))?;
        let token = std::env::var(HOOK_TOKEN_ENV).map_err(|_| missing(HOOK_TOKEN_ENV))?;
        let (read, write) = fds
            .split_once(',')
            .and_then(|(r, w)| Some((r.parse::<u32>().ok()?, w.parse::<u32>().ok()?)))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{HOOK_FD_ENV} must be two descriptor numbers, `read,write`"),
                )
            })?;
        let reader = File::open(fd_path(read))?;
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(fd_path(write))?;
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            token,
        })
    }

    /// The token to present.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Sends `lines` as one request and returns the parent's reply lines.
    ///
    /// # Errors
    ///
    /// I/O errors, and [`io::ErrorKind::UnexpectedEof`] when the parent
    /// closed the channel without answering.
    pub fn exchange(&mut self, lines: &[Vec<u8>]) -> io::Result<Vec<Vec<u8>>> {
        write_section(&mut self.writer, lines)?;
        read_section(&mut self.reader)
    }
}

fn fd_path(fd: u32) -> PathBuf {
    PathBuf::from(format!("/dev/fd/{fd}"))
}

/// git's side of the proc-receive protocol: answers the version
/// negotiation (no capabilities, so git sends no push options) and returns
/// the command lines.
///
/// # Errors
///
/// I/O errors, and [`io::ErrorKind::InvalidData`] when git doesn't offer
/// `version=1`.
pub fn negotiate(input: &mut impl Read, output: &mut impl Write) -> io::Result<Vec<Vec<u8>>> {
    let version = read_section(input)?;
    // `version=1`, then optionally a NUL and git's capabilities.
    let offered = version.first().is_some_and(|line| {
        let version = line.split(|b| *b == 0).next().unwrap_or_default();
        version.strip_suffix(b"\n").unwrap_or(version) == b"version=1"
    });
    if !offered {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "git did not offer proc-receive protocol version 1",
        ));
    }
    write_section(output, &[b"version=1\0".to_vec()])?;
    Ok(read_section(input)?.into_iter().map(trim_newline).collect())
}

/// The packs receive-pack is writing for this push: those with the `.keep`
/// file `index-pack --keep` leaves until the push ends, in the quarantine
/// directory when there is one and in `objects/pack` of the repository the
/// hook runs in.
#[must_use]
pub fn new_packs(git_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![git_dir.join("objects/pack")];
    if let Some(quarantine) = std::env::var_os("GIT_QUARANTINE_PATH") {
        dirs.push(PathBuf::from(quarantine).join("pack"));
    }
    let mut packs = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("pack")) && path.with_extension("keep").exists()
            {
                packs.push(std::path::absolute(&path).unwrap_or(path));
            }
        }
    }
    packs.sort();
    packs
}

/// The request lines for `commands` and `packs`.
#[must_use]
pub fn request_lines(token: &str, commands: &[Vec<u8>], packs: &[PathBuf]) -> Vec<Vec<u8>> {
    let mut lines = vec![format!("token {token}").into_bytes()];
    for pack in packs {
        let mut line = b"pack ".to_vec();
        line.extend_from_slice(pack.as_os_str().as_bytes());
        lines.push(line);
    }
    for command in commands {
        let mut line = b"command ".to_vec();
        line.extend_from_slice(command);
        lines.push(line);
    }
    lines
}

/// Relays the parent's reply to git: messages to `messages` (git shows the
/// hook's stderr to the pusher), results as the proc-receive report.
///
/// # Errors
///
/// I/O errors, and an error carrying the parent's reason when it refused
/// the request.
pub fn report(
    reply: &[Vec<u8>],
    output: &mut impl Write,
    messages: &mut impl Write,
) -> io::Result<()> {
    let mut report = Vec::new();
    for line in reply {
        if let Some(text) = line.strip_prefix(b"message ") {
            messages.write_all(text)?;
            messages.write_all(b"\n")?;
        } else if let Some(reason) = line.strip_prefix(b"error ") {
            return Err(io::Error::other(
                String::from_utf8_lossy(reason).into_owned(),
            ));
        } else if line.starts_with(b"ok ") || line.starts_with(b"ng ") {
            report.push(line.clone());
        }
    }
    messages.flush()?;
    write_section(output, &report)
}

/// `openxet proc-receive`: the whole hook, over stdin, stdout and stderr.
/// Returns the process exit code: 0 once git has the report, 1 otherwise,
/// which fails every command of the push.
#[must_use]
pub fn proc_receive_hook() -> u8 {
    match run_hook() {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(io::stderr(), "openxet proc-receive: {e}");
            1
        }
    }
}

fn run_hook() -> io::Result<()> {
    let mut channel = HookChannel::from_env()?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let commands = negotiate(&mut input, &mut output)?;
    let packs = new_packs(Path::new("."));
    let request = request_lines(channel.token(), &commands, &packs);
    let reply = channel.exchange(&request)?;
    report(&reply, &mut output, &mut io::stderr().lock())
}
