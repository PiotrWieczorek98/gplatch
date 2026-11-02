//! Reads the user configuration.

use std::path::PathBuf;

use crate::APP_NAME;

#[derive(Debug, PartialEq)]
pub struct Config {
    pub portal: String,
    pub as_gateway: bool,
    pub fix_openssl: bool,
    pub ignore_tls_errors: bool,
    pub connect_on_start: bool,
    pub auto_reconnect: bool,
}

pub fn parse_config(text: &str) -> Config {
    let mut cfg = Config {
        portal: String::new(),
        as_gateway: false,
        fix_openssl: true,
        ignore_tls_errors: false,
        connect_on_start: false,
        auto_reconnect: false,
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
            "auto_reconnect" => cfg.auto_reconnect = flag,
            _ => {}
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_existing_config_format() {
        let cfg = parse_config(
            "[vpn]\nportal = vpn.example.com\nas_gateway = true\n# fix_openssl = false\n\
             fix_openssl = no\nignore_tls_errors = True\nconnect_on_start = yes\nauto_reconnect = on\n\n[connection]\npoll_interval = 10\n",
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
            }
        );
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg = parse_config("");
        assert!(
            cfg.portal.is_empty() && cfg.fix_openssl && !cfg.as_gateway && !cfg.ignore_tls_errors
        );
        assert!(!cfg.connect_on_start && !cfg.auto_reconnect);
    }
}
