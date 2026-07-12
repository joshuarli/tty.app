use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{read, write};
use rustix::process::{Pid, Signal, kill_process};
use rustix::termios::{Winsize, tcsetwinsize};

pub struct Pty {
    master: OwnedFd,
    child_pid: i32,
}

impl Pty {
    /// Spawn a new PTY with the user's shell.
    pub fn spawn(cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> io::Result<Self> {
        let mut ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols * cell_width,
            ws_ypixel: rows * cell_height,
        };

        let mut master_fd: RawFd = -1;
        // SAFETY: forkpty is called with valid pointers — &mut master_fd receives the
        // master fd, and &mut ws provides the initial window size. The null pointers
        // for name and termp are permitted by the API (no slave name, no termios).
        let pid = unsafe {
            libc::forkpty(
                &mut master_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut ws,
            )
        };

        if pid < 0 {
            return Err(io::Error::last_os_error());
        }

        if pid == 0 {
            // Child process — exec_shell never returns
            Self::exec_shell();
        }

        // Parent: take ownership of the master and set it to non-blocking.
        // SAFETY: master_fd is a valid open file descriptor that we own exclusively.
        // OwnedFd takes ownership and will close it on drop.
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        let flags = fcntl_getfl(&master)?;
        fcntl_setfl(&master, flags | OFlags::NONBLOCK)?;

        Ok(Pty {
            master,
            child_pid: pid,
        })
    }

    fn exec_shell() -> ! {
        // Start in the user's home directory
        if let Some(home) = std::env::var_os("HOME") {
            let _ = std::env::set_current_dir(home);
        }

        // Set TERM and declare a modern terminal so programs like Claude Code
        // use Unicode/rich output instead of ASCII fallbacks.
        // When launched as an app bundle, macOS doesn't source shell profiles,
        // so LANG is often unset. Without a UTF-8 locale, programs like Claude
        // Code fall back to ASCII and Unicode glyphs render as underscores.
        Self::set_child_env("TERM", "xterm-256color");
        Self::set_child_env("COLORTERM", "truecolor");
        Self::set_child_env("TERM_PROGRAM", "tty");
        Self::set_child_env("LANG", "en_US.UTF-8");

        // Get user's shell
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let c_shell = CString::new(shell.as_str()).unwrap();

        // exec the shell as a login shell (prefix with -)
        let shell_name = std::path::Path::new(&shell)
            .file_name()
            .map(|n| format!("-{}", n.to_string_lossy()))
            .unwrap_or_else(|| "-zsh".to_string());
        let c_name = CString::new(shell_name).unwrap();

        let args: [*const libc::c_char; 2] = [c_name.as_ptr(), std::ptr::null()];

        // SAFETY: c_shell and c_name are valid CStrings kept alive on the stack.
        // args is a null-terminated array of pointers to them. execvp replaces the
        // process image on success; on failure we _exit immediately.
        unsafe {
            libc::execvp(c_shell.as_ptr(), args.as_ptr());
            libc::_exit(1);
        }
    }

    fn set_child_env(key: &str, value: &str) {
        let key = CString::new(key).unwrap();
        let value = CString::new(value).unwrap();
        // SAFETY: setenv copies both strings into the process environment.
        // The CStrings only need to remain valid for the duration of this call.
        unsafe {
            libc::setenv(key.as_ptr(), value.as_ptr(), 1);
        }
    }

    /// The raw file descriptor for the PTY master (for kqueue registration, etc.)
    pub fn fd(&self) -> std::os::fd::RawFd {
        self.master.as_raw_fd()
    }

    /// Read from the PTY master. Returns bytes read, or Err for actual errors.
    /// Returns Ok(0) on true EOF (shell exited).
    /// Returns Err(WouldBlock) when no data is available.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        // rustix validates the descriptor and bounds the read to buf.len().
        Ok(read(&self.master, buf)?)
    }

    /// Write to the PTY master.
    pub fn write(&self, data: &[u8]) -> io::Result<usize> {
        Ok(write(&self.master, data)?)
    }

    /// Resize the PTY.
    pub fn resize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols * cell_width,
            ws_ypixel: rows * cell_height,
        };
        // rustix supplies the platform-specific TIOCSWINSZ encoding and validates
        // the descriptor before updating the PTY dimensions.
        let _ = tcsetwinsize(self.master.as_fd(), ws);
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // SIGHUP signals the child to terminate gracefully.
        if let Some(pid) = Pid::from_raw(self.child_pid) {
            let _ = kill_process(pid, Signal::HUP);
        }
    }
}
