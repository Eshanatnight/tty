use nix::pty::ForkptyResult;
use nix::{errno::Errno, unistd::Pid};
use std::{
    ffi::CStr,
    os::fd::{AsRawFd, OwnedFd},
};
use itertools::Itertools as _;

use ansi::{AnsiParser, TerminalOutput};

mod ansi;

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
                let args: &[&[u8]] = &[b"bash\0", b"--noprofile\0", b"--norc\0"];

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

fn cursor_to_buffer_position(cursor_pos: &CursorPos, buf: &[u8]) -> usize {
    let line_start = buf
        .split(|b| *b == b'\n')
        .take(cursor_pos.y)
        .fold(0, |acc, item| acc + item.len() + 1);
    line_start + cursor_pos.x
}

/// Inserts data at position in buf, extending if necessary
fn insert_data_at_position(data: &[u8], pos: usize, buf: &mut Vec<u8>) {
    assert!(
        pos <= buf.len(),
        "assume pos is never more than 1 past the end of the buffer"
    );

    if pos >= buf.len() {
        assert_eq!(pos, buf.len());
        buf.extend_from_slice(data);
        return;
    }

    let amount_that_fits = buf.len() - pos;
    let (data_to_copy, data_to_push): (&[u8], &[u8]) = if amount_that_fits > data.len() {
        (&data, &[])
    } else {
        data.split_at(amount_that_fits)
    };

    buf[pos..pos + data_to_copy.len()].copy_from_slice(data_to_copy);
    buf.extend_from_slice(data_to_push);
}

#[derive(Clone)]
pub struct CursorPos {
    pub x: usize,
    pub y: usize,
}

pub struct TerminalEmulator {
    output_buf: AnsiParser,
    buf: Vec<u8>,
    cursor_pos: CursorPos,
    fd: OwnedFd,
}

impl TerminalEmulator {
    pub fn new() -> TerminalEmulator {
        let fd = spawn_shell();
        set_nonblock(&fd);

        TerminalEmulator {
            output_buf: AnsiParser::new(),
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
            let parsed = self.output_buf.push(incoming);
            for segment in parsed {
                match segment {
                    TerminalOutput::Data(data) => {
                    let output_start = cursor_to_buffer_position(&self.cursor_pos, &self.buf);
                    insert_data_at_position(&data, output_start, &mut self.buf);
                        update_cursor(&data, &mut self.cursor_pos);
                        // self.buf.extend_from_slice(&data);
                    }
                    TerminalOutput::SetCursorPos { x, y } => {
                        if let Some(x) = x {
                            self.cursor_pos.x = x - 1;
                        }
                        if let Some(y) = y {
                            self.cursor_pos.y = y - 1;
                        }
                    }
                    TerminalOutput::ClearForwards => {
                        let buf_pos = cursor_to_buffer_position(&self.cursor_pos, &self.buf);
                        self.buf = self.buf[..buf_pos].to_vec();
                    }
                    TerminalOutput::ClearBackwards => {
                        // FIXME: Write a test to check expected behavior here, might expect
                        // existing content to stay in the same position
                        let buf_pos = cursor_to_buffer_position(&self.cursor_pos, &self.buf);
                        self.buf = self.buf[buf_pos..].to_vec();
                    }
                    TerminalOutput::ClearAll => {
                        self.buf.clear();
                    }
                    TerminalOutput::Invalid => {}
                }
            }
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


#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_cursor_data_insert() {
        let mut buf = Vec::new();
        insert_data_at_position(b"asdf", 0, &mut buf);
        assert_eq!(buf, b"asdf");

        insert_data_at_position(b"123", 0, &mut buf);
        assert_eq!(buf, b"123f");

        insert_data_at_position(b"xyzw", 4, &mut buf);
        assert_eq!(buf, b"123fxyzw");

        insert_data_at_position(b"asdf", 2, &mut buf);
        assert_eq!(buf, b"12asdfzw");
    }
}