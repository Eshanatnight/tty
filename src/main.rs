mod utils;

use std::ffi::CStr;
use std::os::fd::{AsRawFd, OwnedFd};

use anyhow::anyhow;
use eframe::egui::{self, RichText};
use itertools::Itertools;
use nix::errno::Errno;
use nix::fcntl;
use nix::pty;
use nix::pty::ForkptyResult;
use nix::unistd::Pid;

use utils::{replace_font, add_font};


fn main() -> anyhow::Result<()> {
    let fd = unsafe {
        let fork_res = pty::forkpty(None, None)?;

        let _child: Pid;
        let _master: OwnedFd;
        match fork_res {
            ForkptyResult::Parent { child, master } => {
                _master = master;
                _child = child;
            }
            ForkptyResult::Child => {
                let shell = CStr::from_bytes_until_nul(b"/bin/bash\0")?;

                let args: &[&[u8]] = &[b"--noprofile\0"];
                let args = args
                    .iter()
                    .map(|value| {
                        CStr::from_bytes_with_nul(value).expect("Should Have Null terminator")
                    })
                    .collect_vec();

                // need to unwrap here because execvp never returns
                nix::unistd::execvp(shell, &args).unwrap();
                return Ok(());
            }
        }

        _master
    };

    let native_options = eframe::NativeOptions::default();
    let _ = eframe::run_native(
        "tty",
        native_options,
        Box::new(|cc| Ok(Box::new(Tty::new(cc, fd)))),
    );

    Ok(())
}

struct Tty {
    fd: OwnedFd,
    buffer: Vec<u8>,
}

impl Tty {
    fn new(cc: &eframe::CreationContext<'_>, fd: OwnedFd) -> Self {
        let flags = nix::fcntl::fcntl(fd.as_raw_fd(), fcntl::FcntlArg::F_GETFL)
            .expect("Should Be a valid fd here");
        let mut flags = fcntl::OFlag::from_bits(flags & fcntl::OFlag::O_ACCMODE.bits())
            .ok_or(anyhow!("Is not a vaild fd flags"))
            .expect("Should be a valid flag here");

        flags.set(fcntl::OFlag::O_NONBLOCK, true);

        nix::fcntl::fcntl(fd.as_raw_fd(), fcntl::FcntlArg::F_SETFL(flags))
            .expect("Should not error");

        // TODO: Figure out font ligetures
        replace_font(&cc.egui_ctx);
        add_font(&cc.egui_ctx);

        Self {
            fd,
            buffer: Vec::new(),
        }
    }
}

impl eframe::App for Tty {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut buf = vec![0u8; 4096];

        match nix::unistd::read(self.fd.as_raw_fd(), &mut buf) {
            Ok(sz) => {
                self.buffer.extend_from_slice(&buf[..sz]);
            }
            Err(err) => {
                if err != Errno::EAGAIN {
                    println!("Error reading from fd: {:?}", err);
                }
            }
        };

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.input(|input_reader| {
                for event in &input_reader.events {
                    let egui::Event::Text(text) = event else {
                        continue;
                    };
                    let bytes = text.as_bytes();
                    let mut to_write: &[u8] = bytes;

                    while !to_write.is_empty() {
                        let written = nix::unistd::write(&self.fd, to_write)
                            .expect("Should be able to write");

                        to_write = &to_write[written..];
                    }
                }
            });

            ui.label(RichText::new(String::from_utf8_lossy(&self.buffer)));
        });
    }
}
