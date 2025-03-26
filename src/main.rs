mod utils;

use anyhow::anyhow;
use eframe::egui::{self, Color32, Rect, RichText};
use itertools::Itertools;
use nix::errno::Errno;
use nix::fcntl;
use nix::pty;
use nix::pty::ForkptyResult;
use nix::unistd::Pid;
use std::ffi::CStr;
use std::os::fd::{AsRawFd, OwnedFd};

use utils::{add_font, character_to_screen_pos, get_char_size, replace_font};

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

                let args: &[&[u8]] = &[b"bash\0", b"--noprofile\0"];
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
    cursor_pos: (usize, usize),
    char_size: Option<(f32, f32)>,
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
            char_size: None,
            cursor_pos: (0, 0),
        }
    }
}

impl eframe::App for Tty {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.char_size.is_none() {
            self.char_size = Some(get_char_size(ctx));
            println!("Char Size: {:?}", self.char_size);
        }
        let mut buf = vec![0u8; 4096];

        match nix::unistd::read(self.fd.as_raw_fd(), &mut buf) {
            Ok(sz) => {
                let incomming = &buf[..sz];
                for c in incomming {
                    match c {
                        b'\n' => self.cursor_pos = (0, self.cursor_pos.1 + 1),
                        _ => self.cursor_pos = (self.cursor_pos.0 + 1, self.cursor_pos.1),
                    }
                }
                self.buffer.extend_from_slice(incomming);
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
                    let text = match event {
                        egui::Event::Text(text) => text,
                        egui::Event::Key {
                            key: egui::Key::Enter,
                            pressed: true,
                            ..
                        } => "\n",

                        // egui::Event::Copy => todo!(),
                        // egui::Event::Cut => todo!(),
                        // egui::Event::Paste(_) => todo!(),
                        // egui::Event::PointerMoved(pos2) => todo!(),
                        // egui::Event::MouseMoved(vec2) => todo!(),
                        // egui::Event::PointerButton { pos, button, pressed, modifiers } => todo!(),
                        // egui::Event::PointerGone => todo!(),
                        // egui::Event::Zoom(_) => todo!(),
                        // egui::Event::Ime(ime_event) => todo!(),
                        // egui::Event::Touch { device_id, id, phase, pos, force } => todo!(),
                        // egui::Event::MouseWheel { unit, delta, modifiers } => todo!(),
                        // egui::Event::WindowFocused(_) => todo!(),
                        // egui::Event::AccessKitActionRequest(action_request) => todo!(),
                        // egui::Event::Screenshot { viewport_id, user_data, image } => todo!(),
                        _ => "",
                    };

                    // let egui::Event::Text(text) = event else {
                    //     continue;
                    // };
                    let bytes = text.as_bytes();
                    let mut to_write: &[u8] = bytes;

                    while !to_write.is_empty() {
                        let written = nix::unistd::write(&self.fd, to_write)
                            .expect("Should be able to write");

                        to_write = &to_write[written..];
                    }
                }
            });

            let resp = ui.label(RichText::new(String::from_utf8_lossy(&self.buffer)));
            let bottom = resp.rect.bottom();
            let left = resp.rect.left();
            let painter = ui.painter();
            let char_size = self.char_size.as_ref().unwrap();
            let cursor_offsets = character_to_screen_pos(&self.cursor_pos, char_size, &self.buffer);

            painter.rect_filled(
                Rect::from_min_size(
                    egui::pos2(left + cursor_offsets.0, bottom + cursor_offsets.1),
                    egui::vec2(char_size.0, char_size.1),
                ),
                0.0,
                Color32::GRAY,
            )
        });
    }
}
