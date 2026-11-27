//! Reads the user configuration.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::APP_NAME;

#[derive(Debug, PartialEq)]
pub struct Config {
    pub portal: String,
    pub as_gateway: bool,
    pub fix_openssl: bool,
    pub ignore_tls_errors: bool,
    pub connect_on_start: bool,
    pub auto_reconnect: bool,
    pub browser: String,
    pub script: String,
    /// Keys the parser does not know, usually typos, so the user can be told.
    pub unknown: Vec<String>,
}

/// Written by "Open config" when no config exists, so every setting is visible. Every value is a default.
pub const DEFAULT_CONFIG: &str = "\
[vpn]
# Address of your VPN portal or gateway (required)
portal =
# Connect to the address as a gateway instead of a portal
as_gateway = false
# Start connecting as soon as GPLatch launches
connect_on_start = false
# Reconnect when a tunnel that was up for at least a minute drops
auto_reconnect = false
# Allow servers that need legacy OpenSSL compatibility
fix_openssl = true
# Ignore TLS certificate errors. Only use this if you trust the server.
ignore_tls_errors = false
# Sign in through an external browser instead of the built-in window:
# default, firefox or chrome. Empty uses the built-in window.
browser =
# Route only these subnets and hosts through the VPN, separated by spaces or commas,
# for example: 10.0.0.0/8, intranet.example.com. Needs vpn-slice. Empty routes everything.
split =
# Run this vpnc-script instead of the default one. Asks for your password on connect.
# Ignored when split is set.
script =
";

pub fn parse_config(text: &str) -> Config {
    let mut cfg = Config {
        portal: String::new(),
        as_gateway: false,
        fix_openssl: true,
        ignore_tls_errors: false,
        connect_on_start: false,
        auto_reconnect: false,
        browser: String::new(),
        script: String::new(),
        unknown: vec![],
    };
    let mut split: Vec<&str> = vec![];
    for line in text.lines() {
        if line.trim_start().starts_with(['#', ';']) {
            continue;
        }
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
            "auto_reconnect" => cfg.auto_reconnect = flag,
            "browser" => cfg.browser = value.to_string(),
            "script" => cfg.script = value.to_string(),
            "split" => {
                split = value
                    .split([',', ' ', '\t'])
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            other => cfg.unknown.push(other.to_string()),
        }
    }
    if !split.is_empty() {
        cfg.script = format!("/usr/bin/vpn-slice {}", split.join(" "));
    }
    cfg
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join(APP_NAME)
}

/// The per-user autostart entry, which overrides the system-wide one of the same name.
pub fn autostart_override() -> PathBuf {
    config_dir()
        .with_file_name("autostart")
        .join(format!("{APP_NAME}.desktop"))
}

pub fn starts_on_login(entry: &Path) -> bool {
    !fs::read_to_string(entry)
        .is_ok_and(|text| text.lines().any(|line| line.trim() == "Hidden=true"))
}

/// Hides the system-wide autostart entry for this user, or removes that override.
pub fn set_start_on_login(entry: &Path, on: bool) -> io::Result<()> {
    if on {
        match fs::remove_file(entry) {
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            done => done,
        }
    } else {
        fs::create_dir_all(entry.parent().expect("entry is inside a directory"))?;
        // A full copy, like KDE writes, stays valid if a desktop settings app later flips Hidden.
        let system = include_str!("../packaging/gplatch.desktop");
        fs::write(entry, format!("{system}Hidden=true\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_existing_config_format() {
        let cfg = parse_config(
            "[vpn]\nportal = vpn.example.com\nas_gateway = true\n# fix_openssl = false\n\
             fix_openssl = no\nignore_tls_errors = True\nconnect_on_start = yes\nauto_reconnect = on\nbrowser = default\nscript = vpn-slice 10.0.0.0/8 a.example\n\n[connection]\npoll_interval = 10\n",
        );
        assert_eq!(
            cfg,
            Config {
                portal: "vpn.example.com".into(),
                as_gateway: true,
                fix_openssl: false,
                ignore_tls_errors: true,
                connect_on_start: true,
                auto_reconnect: true,
                browser: "default".into(),
                script: "vpn-slice 10.0.0.0/8 a.example".into(),
                unknown: vec!["poll_interval".into()],
            }
        );
    }

    #[test]
    fn start_on_login_round_trip() {
        let dir = std::env::temp_dir().join(format!("{APP_NAME}-autostart-{}", std::process::id()));
        let entry = dir.join("autostart").join("gplatch.desktop");
        assert!(starts_on_login(&entry), "no override");
        set_start_on_login(&entry, false).unwrap();
        assert!(!starts_on_login(&entry), "hidden");
        let copy = fs::read_to_string(&entry).unwrap();
        assert!(copy.contains("Exec=gplatch") && copy.ends_with("\nHidden=true\n"));
        set_start_on_login(&entry, true).unwrap();
        set_start_on_login(&entry, true).unwrap();
        assert!(starts_on_login(&entry), "override removed");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg = parse_config("");
        assert!(
            cfg.portal.is_empty() && cfg.fix_openssl && !cfg.as_gateway && !cfg.ignore_tls_errors
        );
        assert!(!cfg.connect_on_start && !cfg.auto_reconnect && cfg.browser.is_empty());
        assert!(cfg.script.is_empty());
    }

    #[test]
    fn split_becomes_a_vpn_slice_script() {
        let cfg = parse_config("script = /x\nsplit = 172.27.0.0/16,  10.0.0.0/8 ,db.example.com\n");
        assert_eq!(
            cfg.script,
            "/usr/bin/vpn-slice 172.27.0.0/16 10.0.0.0/8 db.example.com"
        );
        assert_eq!(parse_config("script = /x\nsplit =\n").script, "/x");
    }

    #[test]
    fn reports_unknown_keys() {
        let cfg = parse_config("auto_reconect = true\n; portal_x = y\n  # b = c\n");
        assert_eq!(cfg.unknown, ["auto_reconect"]);
        assert!(!cfg.auto_reconnect);
    }

    #[test]
    fn default_config_file_matches_defaults() {
        assert_eq!(parse_config(DEFAULT_CONFIG), parse_config(""));
    }
}
