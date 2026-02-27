use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use libc::{self, winsize};

pub struct Pty {
    master: OwnedFd,
    child_pid: libc::pid_t,
}

impl Pty {
    /// Spawn a new PTY with the user's shell.
    pub fn spawn(cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> io::Result<Self> {
        let mut ws = winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols * cell_width,
            ws_ypixel: rows * cell_height,
        };

        let mut master_fd: RawFd = -1;
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
            // Child process
            Self::exec_shell();
            // exec_shell never returns
            unreachable!();
        }

        // Parent: set master to non-blocking
        unsafe {
            let flags = libc::fcntl(master_fd, libc::F_GETFL);
            libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };

        Ok(Pty {
            master,
            child_pid: pid,
        })
    }

    fn exec_shell() -> ! {
        // Set TERM
        let term = CString::new("TERM=xterm-256color").unwrap();
        unsafe { libc::putenv(term.as_ptr() as *mut _) };

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

        unsafe {
            libc::execvp(c_shell.as_ptr(), args.as_ptr());
            // If exec fails, exit
            libc::_exit(1);
        }
    }

    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    /// Read from the PTY master. Returns bytes read, or Err for actual errors.
    /// Returns Ok(0) on EAGAIN (no data available).
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe {
            libc::read(
                self.master.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(n as usize)
    }

    /// Write to the PTY master.
    pub fn write(&self, data: &[u8]) -> io::Result<usize> {
        let n = unsafe {
            libc::write(
                self.master.as_raw_fd(),
                data.as_ptr() as *const libc::c_void,
                data.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    /// Resize the PTY.
    pub fn resize(&self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        let ws = winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols * cell_width,
            ws_ypixel: rows * cell_height,
        };
        unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }
    }

    /// Check if the child process is still alive.
    pub fn is_alive(&self) -> bool {
        unsafe {
            let mut status: libc::c_int = 0;
            let result = libc::waitpid(self.child_pid, &mut status, libc::WNOHANG);
            result == 0
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Kill child if still alive
        unsafe {
            libc::kill(self.child_pid, libc::SIGHUP);
        }
    }
}

/// Wait for PTY to be readable using kqueue.
pub fn wait_readable(fd: RawFd, timeout_ms: i64) -> io::Result<bool> {
    unsafe {
        let kq = libc::kqueue();
        if kq < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut change = libc::kevent {
            ident: fd as usize,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };

        let timeout = libc::timespec {
            tv_sec: timeout_ms / 1000,
            tv_nsec: (timeout_ms % 1000) * 1_000_000,
        };

        let mut event: libc::kevent = std::mem::zeroed();
        let n = libc::kevent(
            kq,
            &change,
            1,
            &mut event,
            1,
            &timeout,
        );
        libc::close(kq);

        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n > 0)
    }
}
