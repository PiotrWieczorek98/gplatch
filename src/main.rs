//! GPLatch: a system tray client for GlobalProtect VPNs, built on gpclient.

mod config;
mod tray;
mod vpn;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use ksni::blocking::TrayMethods;

use config::{Config, autostart_override, config_dir, parse_config, starts_on_login};
use tray::{Cmd, VpnTray};
use vpn::{State, Vpn};

const APP_NAME: &str = "gplatch";
const CONFIG_FILE: &str = "config.ini";
const LOG_FILE: &str = "gplatch.log";
const POLL: Duration = Duration::from_secs(1);
/// How often the connection details refresh while connected.
const INFO_REFRESH: Duration = Duration::from_secs(60);
/// A connection that drops sooner than this is not retried, so a failing setup cannot loop.
const MIN_UPTIME_FOR_RECONNECT: Duration = Duration::from_secs(60);
/// Reconnect attempts after a drop, spaced out so the network can come back after sleep.
const RECONNECT_TRIES: u32 = 3;
const RECONNECT_DELAY: Duration = Duration::from_secs(10);
/// How long before the session expires to warn, so the user can reconnect at a good moment.
const EXPIRY_WARNING: Duration = Duration::from_secs(10 * 60);

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

/// Opens the file in the default app, creating it with `initial` if it does not exist yet.
fn open(path: PathBuf, initial: &'static str) {
    thread::spawn(move || {
        // Owner-only, like the log, since both files can reveal the portal and account.
        let created = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => file.write_all(initial.as_bytes()),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(err) => Err(err),
        };
        if let Err(err) = created {
            notify(format!("Could not create {}: {err}", path.display()));
            return;
        }
        let opened = Command::new("xdg-open").arg(&path).status();
        if !opened.is_ok_and(|status| status.success()) {
            notify(format!("Could not open {}", path.display()));
        }
    });
}

/// Reads the config, warning about unknown settings and a missing portal.
fn load_config() -> Config {
    let path = config_dir().join(CONFIG_FILE);
    let cfg = parse_config(&fs::read_to_string(&path).unwrap_or_default());
    if !cfg.unknown.is_empty() {
        notify(format!(
            "Unknown setting {} in {}. Check the spelling.",
            cfg.unknown.join(", "),
            path.display()
        ));
    }
    if cfg.portal.is_empty() {
        notify(format!(
            "No portal set. Add \"portal = your.host\" to {}.",
            path.display()
        ));
    }
    cfg
}

/// Connects with the current config, so edits apply without a restart.
fn connect(vpn: &mut Vpn) {
    vpn.cfg = load_config();
    if !vpn.cfg.portal.is_empty() {
        vpn.connect();
    }
}

#[derive(Debug, PartialEq)]
enum Down {
    Ignore,
    Alert,
    Reconnect,
}

/// Decides how to react when the tunnel goes from `last` to off.
///
/// `retrying` is true while reconnect attempts remain after a drop.
fn on_down(
    last: State,
    stopping: bool,
    auto_reconnect: bool,
    uptime: Duration,
    retrying: bool,
) -> Down {
    if last == State::Off || stopping {
        Down::Ignore
    } else if auto_reconnect
        && (retrying || last == State::Connected && uptime >= MIN_UPTIME_FOR_RECONNECT)
    {
        // Only a drop of a working tunnel starts retries; a failed first login stays failed.
        Down::Reconnect
    } else {
        Down::Alert
    }
}

fn run(mut vpn: Vpn, rx: Receiver<Cmd>, handle: ksni::blocking::Handle<VpnTray>) {
    let refresh = |vpn: &Vpn, state| {
        let info = if state == State::Connected {
            vpn.info()
        } else {
            vec![]
        };
        handle.update(|tray| {
            tray.state = state;
            tray.info = info;
        });
    };
    let mut last = vpn.state();
    refresh(&vpn, last);
    let mut refreshed = Instant::now();
    let mut connected_at = Instant::now();
    let mut retries_left = 0;
    let mut retry_at: Option<Instant> = None;
    let mut expiry_warned = false;
    if vpn.cfg.connect_on_start && !vpn.cfg.portal.is_empty() && last == State::Off {
        vpn.connect();
    }
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Cmd::Toggle) => {
                // A manual choice ends any pending reconnect.
                retries_left = 0;
                retry_at = None;
                if last == State::Off {
                    connect(&mut vpn);
                } else {
                    vpn.disconnect();
                }
            }
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
            let down = on_down(
                last,
                vpn.stopping,
                vpn.cfg.auto_reconnect,
                connected_at.elapsed(),
                retries_left > 0,
            );
            vpn.stopping = false;
            vpn.cancel_pending = false;
            match down {
                Down::Ignore => {}
                Down::Alert => notify(format!("VPN is down, see {}", vpn.log_path.display())),
                Down::Reconnect => {
                    if retries_left == 0 {
                        retries_left = RECONNECT_TRIES;
                        notify("VPN dropped, reconnecting".into());
                    }
                    retries_left -= 1;
                    retry_at = Some(Instant::now() + RECONNECT_DELAY);
                }
            }
        }
        if retry_at.is_some_and(|at| Instant::now() >= at) {
            retry_at = None;
            connect(&mut vpn);
        }
        if state == State::Connected && last != State::Connected {
            connected_at = Instant::now();
            retries_left = 0;
            expiry_warned = false;
        }
        // Periodic refreshes keep the uptime current and pick up session info logged late.
        if state != last || (state == State::Connected && refreshed.elapsed() >= INFO_REFRESH) {
            refresh(&vpn, state);
            refreshed = Instant::now();
            if state == State::Connected
                && !expiry_warned
                && vpn
                    .session_left()
                    .is_some_and(|left| left <= EXPIRY_WARNING)
            {
                expiry_warned = true;
                notify(
                    "VPN session expires in under 10 minutes. Reconnect to start a new one.".into(),
                );
            }
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

    let cfg = load_config();
    let (tx, rx) = channel();
    let tray = VpnTray {
        state: State::Off,
        info: vec![],
        start_on_login: starts_on_login(&autostart_override()),
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

    let vpn = Vpn::new(cfg, dir.join(LOG_FILE));
    run(vpn, rx, handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reacts_to_the_tunnel_going_down() {
        let long = MIN_UPTIME_FOR_RECONNECT;
        let short = Duration::ZERO;
        assert_eq!(
            on_down(State::Off, false, true, long, false),
            Down::Ignore,
            "already off"
        );
        assert_eq!(
            on_down(State::Connected, true, true, long, false),
            Down::Ignore,
            "user disconnected"
        );
        assert_eq!(
            on_down(State::Connected, false, false, long, false),
            Down::Alert,
            "reconnect disabled"
        );
        assert_eq!(
            on_down(State::Connected, false, true, long, false),
            Down::Reconnect,
            "dropped"
        );
        assert_eq!(
            on_down(State::Connected, false, true, short, false),
            Down::Alert,
            "dropped at once"
        );
        assert_eq!(
            on_down(State::Connecting, false, true, long, false),
            Down::Alert,
            "login failed"
        );
        assert_eq!(
            on_down(State::Connecting, false, true, short, true),
            Down::Reconnect,
            "reconnect attempt failed, tries left"
        );
        assert_eq!(
            on_down(State::Connecting, true, true, short, true),
            Down::Ignore,
            "user cancelled a reconnect"
        );
    }
}
