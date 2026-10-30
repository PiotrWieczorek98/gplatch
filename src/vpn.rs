//! Manages the gpauth and gpclient processes and reports the connection state.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::config::Config;
use crate::notify;

/// PID file written by gpclient once the tunnel is established and removed on disconnect.
const GPCLIENT_LOCK: &str = "/run/gpclient.lock";
const MAX_LOG_BYTES: u64 = 1_000_000;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Off,
    Connecting,
    Connected,
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

/// Builds the arguments for the privileged connection.
///
/// The resulting command line must match the pattern in `packaging/49-gplatch.rules`;
/// otherwise polkit falls back to a password prompt.
fn connect_args(cfg: &Config) -> Vec<&str> {
    let mut args = tls_args(cfg);
    args.extend(["connect", "--cookie-on-stdin"]);
    if cfg.as_gateway {
        args.push("--as-gateway");
    }
    args.push(&cfg.portal);
    args
}

/// Reports whether the process named in the lock file is a running gpclient.
///
/// Checking the process name guards against a stale lock file whose PID has been reused.
fn tunnel_up(lock: &Path) -> bool {
    let Ok(pid) = fs::read_to_string(lock) else {
        return false;
    };
    let comm = format!("/proc/{}/comm", pid.trim());
    fs::read_to_string(comm).is_ok_and(|name| name.trim() == "gpclient")
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

fn has_exited(child: &mut Option<Child>) -> bool {
    child
        .as_mut()
        .is_some_and(|child| !matches!(child.try_wait(), Ok(None)))
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Tracks the processes behind the current connection.
pub struct Vpn {
    pub cfg: Config,
    pub log_path: PathBuf,
    auth: Option<Child>,
    child: Option<Child>,
    /// The user requested the shutdown, so the next transition to off raises no alert.
    pub stopping: bool,
    /// A cancel arrived before the tunnel was up and must be applied once it is.
    pub cancel_pending: bool,
}

impl Vpn {
    pub fn new(cfg: Config, log_path: PathBuf) -> Self {
        Self {
            cfg,
            log_path,
            auth: None,
            child: None,
            stopping: false,
            cancel_pending: false,
        }
    }

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

    /// Starts the login and the tunnel as a pipeline.
    ///
    /// gpauth runs unprivileged and passes its result over a pipe to gpclient, the only
    /// component that runs as root. Separate process groups keep a terminal Ctrl-C from
    /// reaching either process and leave the tunnel running if the tray exits.
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
                stop(auth);
                Err(err)
            }
        }
    }

    pub fn connect(&mut self) {
        if let Err(err) = self.spawn_connection() {
            notify(format!("Could not start the VPN: {err}"));
        }
    }

    /// Stops the connection at whatever stage it has reached.
    ///
    /// gpclient runs as root, so the tray cannot signal it directly. During login, killing
    /// gpauth closes gpclient's stdin and it exits. With the tunnel up, `gpclient disconnect`
    /// performs a clean logout. Between those stages, the cancel waits for the tunnel.
    pub fn disconnect(&mut self) {
        self.stopping = true;
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

    pub fn reap(&mut self) {
        if has_exited(&mut self.auth) {
            self.auth = None;
        }
        if has_exited(&mut self.child) {
            self.child = None;
            // Without gpclient the login result has no consumer, so close the login window.
            if let Some(auth) = self.auth.take() {
                stop(auth);
            }
        }
    }

    /// Derives the state from the system so tunnels started elsewhere are detected too.
    pub fn state(&self) -> State {
        if tunnel_up(Path::new(GPCLIENT_LOCK)) {
            State::Connected
        } else if self.child.is_some() {
            State::Connecting
        } else {
            State::Off
        }
    }

    /// Waits up to five seconds, keeping a stuck gpclient from blocking tray shutdown.
    pub fn wait_for_exit(&mut self) {
        for _ in 0..50 {
            self.reap();
            if self.child.is_none() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::APP_NAME;
    use crate::config::parse_config;

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
