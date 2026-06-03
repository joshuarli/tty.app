use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::RawFd;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success())
}

fn drain_fd(fd: RawFd, timeout: Duration) -> Vec<u8> {
    let mut out = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        let remaining = timeout.saturating_sub(start.elapsed());
        let mut readfds = unsafe { std::mem::zeroed::<libc::fd_set>() };
        unsafe {
            libc::FD_ZERO(&mut readfds);
            libc::FD_SET(fd, &mut readfds);
        }

        let mut tv = libc::timeval {
            tv_sec: remaining.as_secs().min(1) as libc::time_t,
            tv_usec: remaining.subsec_micros() as libc::suseconds_t,
        };

        let ready = unsafe {
            libc::select(
                fd + 1,
                &mut readfds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if ready <= 0 {
            continue;
        }

        let mut buf = [0u8; 65536];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    out
}

fn spawn_tmux_client(socket: &str, config: &str) -> io::Result<(libc::pid_t, RawFd)> {
    let mut master_fd: RawFd = -1;
    let mut ws = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

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
        setenv("TERM", "tmux-256color");
        let args = [
            "tmux",
            "-L",
            socket,
            "-f",
            config,
            "new-session",
            "-s",
            "test",
        ];
        execvp(&args);
    }

    let flags = unsafe { libc::fcntl(master_fd, libc::F_GETFL) };
    unsafe {
        libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    Ok((pid, master_fd))
}

fn setenv(key: &str, value: &str) {
    let key = CString::new(key).unwrap();
    let value = CString::new(value).unwrap();
    unsafe {
        libc::setenv(key.as_ptr(), value.as_ptr(), 1);
    }
}

fn execvp(args: &[&str]) -> ! {
    let cstrings: Vec<CString> = args.iter().map(|arg| CString::new(*arg).unwrap()).collect();
    let mut argv: Vec<*const libc::c_char> = cstrings.iter().map(|arg| arg.as_ptr()).collect();
    argv.push(std::ptr::null());
    unsafe {
        libc::execvp(cstrings[0].as_ptr(), argv.as_ptr());
        libc::_exit(127);
    }
}

#[test]
fn tmux256color_client_with_clipboard_feature_emits_osc52() {
    if !tmux_available() {
        return;
    }

    let unique = format!(
        "tty-tmux-clipboard-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let config = std::env::temp_dir().join(format!("{unique}.conf"));
    fs::write(
        &config,
        "set -g default-terminal \"tmux-256color\"\n\
         set -g set-clipboard external\n\
         set -ga terminal-features \"tmux-256color:Sync:clipboard\"\n",
    )
    .unwrap();

    let config_str = config.to_string_lossy().into_owned();
    let (pid, fd) = spawn_tmux_client(&unique, &config_str).unwrap();
    drain_fd(fd, Duration::from_millis(800));

    let clients = Command::new("tmux")
        .args([
            "-L",
            &unique,
            "list-clients",
            "-F",
            "#{client_termname} #{client_termfeatures}",
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&clients.stdout).contains("clipboard"),
        "tmux client features did not include clipboard: {}",
        String::from_utf8_lossy(&clients.stdout)
    );

    let set_buffer = Command::new("tmux")
        .args(["-L", &unique, "set-buffer", "-w", "hello"])
        .output()
        .unwrap();
    assert!(
        set_buffer.status.success(),
        "tmux set-buffer failed: {}",
        String::from_utf8_lossy(&set_buffer.stderr)
    );

    let output = drain_fd(fd, Duration::from_millis(800));
    Command::new("tmux")
        .args(["-L", &unique, "kill-server"])
        .output()
        .ok();
    unsafe {
        libc::kill(pid, libc::SIGHUP);
        libc::close(fd);
    }
    fs::remove_file(config).ok();

    assert!(
        output.windows(b"\x1B]52;".len()).any(|w| w == b"\x1B]52;"),
        "tmux did not emit OSC 52; output tail: {:?}",
        String::from_utf8_lossy(&output[output.len().saturating_sub(256)..])
    );
    assert!(
        output.windows(b"aGVsbG8=".len()).any(|w| w == b"aGVsbG8="),
        "tmux OSC 52 output did not contain the expected clipboard payload"
    );
}
