use nix::pty::ForkptyResult;
use nix::{errno::Errno, unistd::Pid};
use std::{
    ffi::CStr,
    os::fd::{AsRawFd, OwnedFd},
};
use itertools::Itertools as _;

/// Spawn a shell in a child process and return the file descriptor used for I/O
fn spawn_shell() -> OwnedFd {
    unsafe {
        let res = nix::pty::forkpty(None, None).unwrap();

        let _child: Pid;
        let _master: OwnedFd;

        match res {
            ForkptyResult::Parent { child, master } => {
                _master = master;
                _child = child;
            }
            ForkptyResult::Child => {
                let shell_name = CStr::from_bytes_with_nul(b"/bin/bash\0")
                    .expect("Should always have null terminator");
                let args: &[&[u8]] = &[b"bash\0", b"--noprofile\0"];

                let args: Vec<&'static CStr> = args
                    .iter()
                    .map(|v| {
                        CStr::from_bytes_with_nul(v).expect("Should always have null terminator")
                    })
                    .collect_vec();

                // Temporary workaround to avoid rendering issues
                std::env::remove_var("PROMPT_COMMAND");
                std::env::set_var("PS1", "$ ");
                nix::unistd::execvp(shell_name, &args).unwrap();
                // Should never run
                std::process::exit(1);
            }
        }
        _master
    }
}

fn update_cursor(incoming: &[u8], cursor: &mut CursorPos) {
    for c in incoming {
        match c {
            b'\n' => {
                cursor.x = 0;
                cursor.y += 1;
            }
            _ => {
                cursor.x += 1;
            }
        }
    }
}

fn set_nonblock(fd: &OwnedFd) {
    let flags = nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL).unwrap();
    let mut flags =
        nix::fcntl::OFlag::from_bits(flags & nix::fcntl::OFlag::O_ACCMODE.bits()).unwrap();
    flags.set(nix::fcntl::OFlag::O_NONBLOCK, true);

    nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(flags)).unwrap();
}

#[derive(Clone)]
pub struct CursorPos {
    pub x: usize,
    pub y: usize,
}

pub struct TerminalEmulator {
    buf: Vec<u8>,
    cursor_pos: CursorPos,
    fd: OwnedFd,
}

impl TerminalEmulator {
    pub fn new() -> TerminalEmulator {
        let fd = spawn_shell();
        set_nonblock(&fd);

        TerminalEmulator {
            buf: Vec::new(),
            cursor_pos: CursorPos { x: 0, y: 0 },
            fd,
        }
    }

    pub fn write(&mut self, mut to_write: &[u8]) {
        while !to_write.is_empty() {
            // this is a hack for now
            let written = nix::unistd::write(self.fd.try_clone().unwrap(), to_write).unwrap();
            to_write = &to_write[written..];
        }
    }

    pub fn read(&mut self) {
        let mut buf = vec![0u8; 4096];
        let mut ret = Ok(0);
        while ret.is_ok() {
            ret = nix::unistd::read(self.fd.as_raw_fd(), &mut buf);
            let Ok(read_size) = ret else {
                break;
            };

            let incoming = &buf[0..read_size];
            update_cursor(incoming, &mut self.cursor_pos);
            self.buf.extend_from_slice(incoming);
        }

        if let Err(e) = ret {
            if e != Errno::EAGAIN {
                println!("Failed to read: {e}");
            }
        }
    }

    pub fn data(&self) -> &[u8] {
        &self.buf
    }

    pub fn cursor_pos(&self) -> CursorPos {
        self.cursor_pos.clone()
    }
}
