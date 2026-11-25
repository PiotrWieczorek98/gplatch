<div align="center">
  <img src="packaging/icons/gplatch.svg" width="64" alt="GPLatch icon">
  <h1>GPLatch</h1>
  <p>A free system tray client for GlobalProtect VPNs on Linux.</p>
</div>


## Requirements
- Linux. GPLatch is tested on Arch-based distributions, and packages are only built for
  those. See [Other distributions](#other-distributions).
- A desktop with a StatusNotifierItem system tray. KDE Plasma works out of the box. On
  GNOME, install the AppIndicator extension.
- Membership of the `wheel` group, to connect without a password prompt

## Install

Download the latest `.pkg.tar.zst` file from the
[releases page](https://github.com/PiotrWieczorek98/gplatch/releases), then run:

```sh
sudo pacman -U gplatch-*.pkg.tar.zst
```

To build it yourself instead, run this from a copy of this repository:

```sh
cd packaging
makepkg -si
```

To rebuild after changing the code without bumping the version, use `makepkg -sif`.
Without `-f`, makepkg reinstalls the package it already built.

Either way, this also installs `globalprotect-openconnect`, which provides the VPN
connection.

### Other distributions

These steps are untested. You need Rust 1.89 or newer, `polkit`, `notify-send`, `ip`,
`xdg-open`, and `globalprotect-openconnect` installed with `gpclient` at
`/usr/bin/gpclient`. Then run:

```sh
cargo build --release
sudo install -Dm755 target/release/gplatch -t /usr/local/bin
sudo install -Dm644 packaging/49-gplatch.rules -t /etc/polkit-1/rules.d
sudo install -Dm644 packaging/gplatch.desktop -t /etc/xdg/autostart
sudo install -Dm644 packaging/gplatch.desktop packaging/gpauth.desktop -t /usr/share/applications
sudo install -Dm644 packaging/icons/gplatch.svg -t /usr/share/icons/hicolor/scalable/apps
```

If your distribution has no `wheel` group, such as Debian and Ubuntu, GPLatch asks for
your password each time. To avoid that, change `wheel` in the installed rule to your admin
group, such as `sudo`.

## Setup

Create `~/.config/gplatch/config.ini` with your VPN address, or choose **Open config** from
the tray menu. If the file does not exist yet, it is created listing every setting at its
default value, so you only need to fill in the portal:

```ini
[vpn]
portal = vpn.example.com
```

Then log out and back in, or run `gplatch`. GPLatch starts automatically each time you
log in. To stop that, untick **Start on login** in the tray menu.

## Usage

Click the tray icon and choose **Connect**, then sign in using the window that opens. If
you set `browser`, the sign-in page opens in your browser instead.

While it is connecting, the same menu item changes to **Cancel**. Once you are
connected, it changes to **Disconnect**. Choosing **Quit** also disconnects the VPN.

The top of the menu shows the connection status. While connected, it also shows the
gateway, the IP address the VPN assigned you, how long you have been connected, and when
the session expires.

**Open config** and **Open log** open those files in your default app.

## Settings

These go in the `[vpn]` section of `config.ini`. A config created with **Open config**
already lists them all. Changes apply the next time you connect, except
`connect_on_start`, which applies the next time GPLatch starts.

| Setting | Default | What it does |
|---|---|---|
| `portal` | none | The address of your VPN portal or gateway |
| `as_gateway` | `false` | Connect to the address as a gateway instead of a portal |
| `connect_on_start` | `false` | Start connecting as soon as GPLatch launches |
| `auto_reconnect` | `false` | Reconnect when a tunnel that was up for at least a minute drops |
| `fix_openssl` | `true` | Allow servers that need legacy OpenSSL compatibility |
| `ignore_tls_errors` | `false` | Ignore TLS certificate errors. Only use this if you trust the server. |
| `browser` | none | Sign in through an external browser instead of the built-in window, e.g. `default`, `firefox` or `chrome`. Use it when your sign-in needs a desktop app, such as Okta FastPass. |
| `split` | none | Route only these subnets and hosts through the VPN, separated by spaces or commas. See [Split tunneling](#split-tunneling). |
| `script` | none | Run this [vpnc-script](https://www.infradead.org/openconnect/vpnc-script.html) instead of the default one. Connecting then asks for your password. Ignored when `split` is set. |

## Split tunneling

By default, all your traffic goes through the VPN, including DNS. To send only some
addresses through it, install [vpn-slice](https://github.com/dlenski/vpn-slice) (`vpn-slice`
in the AUR) and list the subnets and hosts that should use the VPN:

```ini
[vpn]
portal = vpn.example.com
split = 10.0.0.0/8, 172.16.0.0/12, jira.example.com
```

Everything else, including DNS, uses your normal connection. vpn-slice looks up the listed
hostnames through the VPN's DNS and adds them to `/etc/hosts`. Anything you do not list
cannot be reached over the VPN.

Changes apply the next time you connect. To try a subnet without reconnecting, run
`sudo ip route add 10.20.0.0/16 dev tun0`; that route lasts until you disconnect.

To find which subnets your organization uses, connect once without `split` and run
`ip route show dev tun0`. If it lists only a default route, add each private address
(10.x, 172.16–31.x or 192.168.x) as you find you need it, using `getent hosts <name>`
while connected.

List hostnames, not URLs: `publication.example.com`, not
`https://publication.example.com/`. Entries may only contain letters, digits and `.`, `:`,
`/` or `-`. Anything else makes connecting ask for your password. Your organization may
not allow split tunneling, so check before you use it.

## Troubleshooting

If a connection fails, choose **Open log** from the tray menu, or check
`~/.config/gplatch/gplatch.log`, for the details.

If your sign-in needs a desktop app, the built-in login window may not be able to reach
it. For example, with Okta FastPass the **Open Okta Verify** button does nothing or the
sign-in keeps asking again. Set `browser = default` to sign in through your browser. If
the browser asks whether the site may access apps or devices on your local network,
allow it.

## Permissions

A VPN needs root access to set up its network connection. GPLatch keeps that as small
as it can:

- The app and the login window run as your normal user.
- Only `gpclient` runs as root, started through `pkexec`.
- The included polkit rule lets `wheel` users in an active local session run only the
  connect and disconnect commands GPLatch needs, without a password. Anything else
  gets the normal password prompt, including connecting with a custom `script`, since
  the script runs as root.

## Releasing

From a clean, up-to-date `main`, run:

```sh
scripts/gplatch-update patch   # or minor, major, or an exact version such as 0.3.0
```

It updates the version in `Cargo.toml`, `Cargo.lock` and `packaging/PKGBUILD`, runs the
tests and clippy, and asks before it commits, tags and pushes. The pushed tag starts the
release workflow, which builds the package and publishes it on the releases page.

## License

GPLatch is licensed under the [GNU General Public License v3.0](LICENSE).

It is an independent project, not affiliated with Palo Alto Networks or GPGUI.
GlobalProtect is a trademark of Palo Alto Networks.
