//! GPLatch: a system tray client for GlobalProtect VPNs, built on gpclient.

mod config;
mod tray;
mod vpn;

use std::fs::{self, File};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::Duration;

use ksni::blocking::TrayMethods;

use config::{config_dir, parse_config};
use tray::{Cmd, VpnTray};
use vpn::{State, Vpn};

const APP_NAME: &str = "gplatch";
const POLL: Duration = Duration::from_secs(1);

fn notify(body: String) {
    thread::spawn(move || {
        let sent = Command::new("notify-send")
            .args(["-a", "GPLatch", "-i", "gplatch", "GPLatch", &body])
            .status();
        if let Err(err) = sent {
            eprintln!("{APP_NAME}: notify-send failed: {err}; message was: {body}");
        }
    });
}

fn run(mut vpn: Vpn, rx: Receiver<Cmd>, handle: ksni::blocking::Handle<VpnTray>) {
    let mut last = vpn.state();
    handle.update(|tray| tray.state = last);
    if vpn.cfg.connect_on_start && !vpn.cfg.portal.is_empty() && last == State::Off {
        vpn.connect();
    }
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Cmd::Toggle) if last == State::Off => vpn.connect(),
            Ok(Cmd::Toggle) => vpn.disconnect(),
            Ok(Cmd::Quit) => {
                if last != State::Off {
                    vpn.disconnect();
                    vpn.wait_for_exit();
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        vpn.reap();
        let state = vpn.state();
        if state == State::Connected && vpn.cancel_pending {
            vpn.cancel_pending = false;
            vpn.disconnect();
        }
        if state == State::Off {
            if last != State::Off && !vpn.stopping {
                notify(format!("VPN is down, see {}", vpn.log_path.display()));
            }
            vpn.stopping = false;
            vpn.cancel_pending = false;
        }
        if state != last {
            handle.update(|tray| tray.state = state);
            last = state;
        }
    }
}

/// Restores default SIGINT handling, which child processes inherit.
///
/// `gpclient disconnect` stops the tunnel with SIGINT. If the launcher blocked that signal,
/// gpclient would inherit the block and never receive the request.
fn reset_sigint() {
    // SAFETY: Called first in `main`, before any other thread exists, and `set` is initialized
    // by `sigemptyset` before use.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
}

fn main() {
    reset_sigint();
    let dir = config_dir();
    if let Err(err) = fs::create_dir_all(&dir) {
        eprintln!("{APP_NAME}: cannot create {}: {err}", dir.display());
        std::process::exit(1);
    }

    // The lock is held for the life of the process and enforces a single instance.
    let instance = File::create(dir.join("instance.lock")).expect("config dir is writable");
    if instance.try_lock().is_err() {
        eprintln!("{APP_NAME}: already running");
        return;
    }

    let config_path = dir.join("config.ini");
    let cfg = parse_config(&fs::read_to_string(&config_path).unwrap_or_default());
    let can_connect = !cfg.portal.is_empty();
    if !can_connect {
        notify(format!(
            "No portal set. Add \"portal = your.host\" to {} and restart.",
            config_path.display()
        ));
    }

    let (tx, rx) = channel();
    let tray = VpnTray {
        state: State::Off,
        can_connect,
        tx,
    };
    // At login the tray can start before the panel. Assuming availability makes it
    // register when the panel appears instead of failing at startup.
    let handle = match tray.assume_sni_available(true).spawn() {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("{APP_NAME}: cannot start tray: {err}");
            std::process::exit(1);
        }
    };

    let vpn = Vpn::new(cfg, dir.join("gplatch.log"));
    run(vpn, rx, handle);
}
