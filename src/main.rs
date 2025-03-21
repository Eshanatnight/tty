use std::ffi::CStr;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;

use eframe::egui;
use nix::pty;
use nix::pty::ForkptyResult;
use nix::unistd::Pid;

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
                let args = [CStr::from_bytes_until_nul(b"--noprofile\0")?];
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
        Box::new(|cc| Ok(Box::new(TTY::new(cc, fd)))),
    );

    Ok(())
}

struct TTY {
    fd: File,
    buffer: Vec<u8>,
}

impl TTY {
    fn new(cc: &eframe::CreationContext<'_>, fd: OwnedFd) -> Self {
        // Customize egui here with cc.egui_ctx.set_fonts and cc.egui_ctx.set_visuals.
        // Restore app state using cc.storage (requires the "persistence" feature).
        // Use the cc.gl (a glow::Context) to create graphics shaders and buffers that you can use
        // for e.g. egui::PaintCallback.
        Self {
            fd: fd.into(),
            buffer: Vec::new(),
        }
    }
}

impl eframe::App for TTY {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let mut buf = vec![0u8; 4096];
        match self.fd.read(&mut buf) {
            Ok(sz) => {
                self.buffer.extend_from_slice(&buf[..sz]);
            }
            Err(err) => {
                println!("Error reading from fd: {:?}", err);
            }
        };

        egui::CentralPanel::default().show(ctx, |ui| {
            unsafe  {
                ui.label(std::str::from_utf8_unchecked(&self.buffer));
            }
        });
    }
}
