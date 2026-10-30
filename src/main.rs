use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::Duration;

use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, Icon, ToolTip, Tray};

const APP_NAME: &str = "gplatch";
// gpclient writes this once the tunnel is up and removes it on disconnect.
const GPCLIENT_LOCK: &str = "/run/gpclient.lock";
const POLL: Duration = Duration::from_secs(1);
const MAX_LOG_BYTES: u64 = 1_000_000;
const ICON_SIZE: i32 = 64;

#[derive(Debug, PartialEq)]
struct Config {
    portal: String,
    as_gateway: bool,
    fix_openssl: bool,
    ignore_tls_errors: bool,
    connect_on_start: bool,
}

fn parse_config(text: &str) -> Config {
    let mut cfg = Config {
        portal: String::new(),
        as_gateway: false,
        fix_openssl: true,
        ignore_tls_errors: false,
        connect_on_start: false,
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let flag = matches!(value.to_lowercase().as_str(), "true" | "yes" | "on" | "1");
        match key.trim() {
            "portal" => cfg.portal = value.to_string(),
            "as_gateway" => cfg.as_gateway = flag,
            "fix_openssl" => cfg.fix_openssl = flag,
            "ignore_tls_errors" => cfg.ignore_tls_errors = flag,
            "connect_on_start" => cfg.connect_on_start = flag,
            _ => {}
        }
    }
    cfg
}

fn tls_args(cfg: &Config) -> Vec<&str> {
    let mut args = vec![];
    if cfg.fix_openssl {
        args.push("--fix-openssl");
    }
    if cfg.ignore_tls_errors {
        args.push("--ignore-tls-errors");
    }
    args
}

fn auth_args(cfg: &Config) -> Vec<&str> {
    let mut args = tls_args(cfg);
    if cfg.as_gateway {
        args.push("--gateway");
    }
    args.push(&cfg.portal);
    args
}

// Must stay in step with the command line packaging/49-gplatch.rules allows.
fn connect_args(cfg: &Config) -> Vec<&str> {
    let mut args = tls_args(cfg);
    args.extend(["connect", "--cookie-on-stdin"]);
    if cfg.as_gateway {
        args.push("--as-gateway");
    }
    args.push(&cfg.portal);
    args
}

// The comm check keeps a stale lock whose pid got reused from reading as connected.
fn tunnel_up(lock: &Path) -> bool {
    let Ok(pid) = fs::read_to_string(lock) else {
        return false;
    };
    let comm = format!("/proc/{}/comm", pid.trim());
    fs::read_to_string(comm).is_ok_and(|name| name.trim() == "gpclient")
}

fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join(APP_NAME)
}

fn open_log(path: &Path) -> std::io::Result<File> {
    if fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        fs::rename(path, path.with_extension("log.1"))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

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

#[derive(Clone, Copy, PartialEq)]
enum State {
    Off,
    Connecting,
    Connected,
}

enum Cmd {
    Toggle,
    Quit,
}

struct VpnTray {
    state: State,
    can_connect: bool,
    tx: Sender<Cmd>,
}

impl Tray for VpnTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        APP_NAME.into()
    }

    fn title(&self) -> String {
        "GPLatch".into()
    }

    fn category(&self) -> Category {
        Category::Communications
    }

    // Sent as pixels so the icon doesn't depend on the desktop having rescanned its icon themes.
    fn icon_pixmap(&self) -> Vec<Icon> {
        let data: &[u8] = match self.state {
            State::Off => include_bytes!("../packaging/icons/gplatch-off.rgba"),
            State::Connecting => include_bytes!("../packaging/icons/gplatch-connecting.rgba"),
            State::Connected => include_bytes!("../packaging/icons/gplatch-connected.rgba"),
        };
        let mut data = data.to_vec();
        data.as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|pixel| pixel.rotate_right(1)); // RGBA to ARGB
        vec![Icon {
            width: ICON_SIZE,
            height: ICON_SIZE,
            data,
        }]
    }

    fn tool_tip(&self) -> ToolTip {
        let status = match self.state {
            State::Off => "Off",
            State::Connecting => "Connecting...",
            State::Connected => "Connected",
        };
        ToolTip {
            title: format!("GPLatch — {status}"),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let label = match self.state {
            State::Off => "Connect",
            State::Connecting => "Cancel",
            State::Connected => "Disconnect",
        };
        vec![
            StandardItem {
                label: label.into(),
                enabled: self.can_connect || self.state != State::Off,
                activate: Box::new(|tray: &mut Self| drop(tray.tx.send(Cmd::Toggle))),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| drop(tray.tx.send(Cmd::Quit))),
                ..Default::default()
            }
            .into(),
        ]
    }
}

struct Vpn {
    cfg: Config,
    log_path: PathBuf,
    auth: Option<Child>,
    child: Option<Child>,
    stopping: bool,
    cancel_pending: bool,
}

impl Vpn {
    fn log_stdio(&self) -> std::io::Result<(File, File)> {
        let log = open_log(&self.log_path)?;
        Ok((log.try_clone()?, log))
    }

    fn pkexec_gpclient(&self, args: &[&str]) -> std::io::Result<Command> {
        let (stdout, stderr) = self.log_stdio()?;
        let mut cmd = Command::new("pkexec");
        cmd.arg("/usr/bin/gpclient")
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        Ok(cmd)
    }

    // Login runs as the user; only the tunnel gets root, fed the login result over a pipe.
    fn spawn_connection(&mut self) -> std::io::Result<()> {
        let (_, stderr) = self.log_stdio()?;
        let mut auth = Command::new("gpauth")
            .args(auth_args(&self.cfg))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .process_group(0)
            .spawn()?;
        let cookie = auth.stdout.take().expect("gpauth stdout is piped");
        let spawned = self
            .pkexec_gpclient(&connect_args(&self.cfg))
            .and_then(|mut cmd| cmd.stdin(cookie).process_group(0).spawn());
        match spawned {
            Ok(child) => {
                self.auth = Some(auth);
                self.child = Some(child);
                Ok(())
            }
            Err(err) => {
                let _ = auth.kill();
                let _ = auth.wait();
                Err(err)
            }
        }
    }

    fn connect(&mut self) {
        if let Err(err) = self.spawn_connection() {
            notify(format!("Could not start the VPN: {err}"));
        }
    }

    fn disconnect(&mut self) {
        self.stopping = true;
        // gpclient is root and can't be signalled; losing gpauth ends its stdin, so it exits.
        if let Some(auth) = &mut self.auth {
            let _ = auth.kill();
        }
        if tunnel_up(Path::new(GPCLIENT_LOCK)) {
            let done = self
                .pkexec_gpclient(&["disconnect"])
                .and_then(|mut cmd| cmd.status());
            if !done.is_ok_and(|status| status.success()) {
                self.stopping = false;
                notify(format!(
                    "Disconnect failed, see {}",
                    self.log_path.display()
                ));
            }
        } else {
            self.cancel_pending = self.child.is_some();
        }
    }

    fn reap(&mut self) {
        if self
            .auth
            .as_mut()
            .is_some_and(|auth| !matches!(auth.try_wait(), Ok(None)))
        {
            self.auth = None;
        }
        if self
            .child
            .as_mut()
            .is_some_and(|child| !matches!(child.try_wait(), Ok(None)))
        {
            self.child = None;
            // Refused by polkit or failed: don't leave a login window that leads nowhere.
            if let Some(mut auth) = self.auth.take() {
                let _ = auth.kill();
                let _ = auth.wait();
            }
        }
    }

    fn state(&self) -> State {
        if tunnel_up(Path::new(GPCLIENT_LOCK)) {
            State::Connected
        } else if self.child.is_some() {
            State::Connecting
        } else {
            State::Off
        }
    }

    fn wait_for_exit(&mut self) {
        for _ in 0..50 {
            self.reap();
            if self.child.is_none() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
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
        // Cancelled after login finished: gpclient can only be stopped once its tunnel is up.
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

// gpclient stops on SIGINT and child processes inherit these settings, so a launcher that
// blocks or ignores SIGINT would otherwise leave a VPN that can never be disconnected.
fn reset_sigint() {
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
    let mut portal = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--portal", Some(value)) => portal = Some(value),
            _ => {
                eprintln!("usage: {APP_NAME} [--portal HOST]");
                std::process::exit(2);
            }
        }
    }

    let dir = config_dir();
    if let Err(err) = fs::create_dir_all(&dir) {
        eprintln!("{APP_NAME}: cannot create {}: {err}", dir.display());
        std::process::exit(1);
    }

    // Held until exit, so a second autostart or launcher click quits quietly.
    let instance = File::create(dir.join("instance.lock")).expect("config dir is writable");
    if instance.try_lock().is_err() {
        eprintln!("{APP_NAME}: already running");
        return;
    }

    let config_path = dir.join("config.ini");
    let mut cfg = parse_config(&fs::read_to_string(&config_path).unwrap_or_default());
    cfg.portal = portal.unwrap_or(cfg.portal);
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
    // Assumed available so a tray started before the panel registers once the panel appears.
    let handle = match tray.assume_sni_available(true).spawn() {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("{APP_NAME}: cannot start tray: {err}");
            std::process::exit(1);
        }
    };

    let vpn = Vpn {
        cfg,
        log_path: dir.join("gplatch.log"),
        auth: None,
        child: None,
        stopping: false,
        cancel_pending: false,
    };
    run(vpn, rx, handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_existing_config_format() {
        let cfg = parse_config(
            "[vpn]\nportal = vpn.example.com\nas_gateway = true\n# fix_openssl = false\n\
             fix_openssl = no\nignore_tls_errors = True\nconnect_on_start = yes\n\n[connection]\npoll_interval = 10\n",
        );
        assert_eq!(
            cfg,
            Config {
                portal: "vpn.example.com".into(),
                as_gateway: true,
                fix_openssl: false,
                ignore_tls_errors: true,
                connect_on_start: true,
            }
        );
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg = parse_config("");
        assert!(
            cfg.portal.is_empty() && cfg.fix_openssl && !cfg.as_gateway && !cfg.ignore_tls_errors
        );
        assert!(!cfg.connect_on_start);
    }

    #[test]
    fn global_flags_go_before_the_subcommand() {
        let cfg = Config {
            portal: "p".into(),
            as_gateway: true,
            ignore_tls_errors: true,
            ..parse_config("")
        };
        assert_eq!(
            connect_args(&cfg),
            [
                "--fix-openssl",
                "--ignore-tls-errors",
                "connect",
                "--cookie-on-stdin",
                "--as-gateway",
                "p"
            ]
        );
        assert_eq!(
            auth_args(&cfg),
            ["--fix-openssl", "--ignore-tls-errors", "--gateway", "p"]
        );
    }

    #[test]
    fn icons_are_64px() {
        for state in [State::Off, State::Connecting, State::Connected] {
            let tray = VpnTray {
                state,
                can_connect: true,
                tx: channel().0,
            };
            assert_eq!(
                tray.icon_pixmap()[0].data.len(),
                (ICON_SIZE * ICON_SIZE * 4) as usize
            );
        }
    }

    #[test]
    fn tunnel_needs_a_live_gpclient_behind_the_lock() {
        let dir = std::env::temp_dir().join(format!("{APP_NAME}-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("lock");
        assert!(!tunnel_up(&lock), "missing lock");

        fs::write(&lock, "not a pid").unwrap();
        assert!(!tunnel_up(&lock), "garbage lock");

        fs::write(&lock, std::process::id().to_string()).unwrap();
        assert!(!tunnel_up(&lock), "pid reused by another program");

        let fake = dir.join("gpclient");
        fs::copy("/usr/bin/sleep", &fake).unwrap();
        let mut child = Command::new(&fake).arg("30").spawn().unwrap();
        fs::write(&lock, format!("{}\n", child.id())).unwrap();
        assert!(tunnel_up(&lock), "live gpclient");

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!tunnel_up(&lock), "stale lock");
        fs::remove_dir_all(&dir).unwrap();
    }
}
