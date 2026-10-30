<div align="center">
  <img src="packaging/icons/gplatch.svg" width="64" alt="GPLatch icon">
  <h1>GPLatch</h1>
  <p>A free system tray client for GlobalProtect VPNs on Arch Linux.</p>
</div>


## Requirements
- Arch Linux or an Arch-based distribution
- A desktop with a system tray. KDE Plasma works out of the box.
- Membership of the `wheel` group, to connect without a password prompt

## Install

From a copy of this repository, run:

```sh
cd packaging
makepkg -si
```

This also installs `globalprotect-openconnect`, which provides the VPN connection.

## Setup

Create `~/.config/gplatch/config.ini` with your VPN address:

```ini
[vpn]
portal = vpn.example.com
```

Then log out and back in, or run `gplatch`. GPLatch starts automatically each time you
log in.

## Usage

Click the tray icon and choose **Connect**, then sign in using the window that opens.

While it is connecting, the same menu item changes to **Cancel**. Once you are
connected, it changes to **Disconnect**. Choosing **Quit** also disconnects the VPN.

## Settings

Add any of these to the `[vpn]` section of `config.ini`, then restart GPLatch.

| Setting | Default | What it does |
|---|---|---|
| `portal` | none | The address of your VPN portal or gateway |
| `as_gateway` | `false` | Connect to the address as a gateway instead of a portal |
| `connect_on_start` | `false` | Start connecting as soon as GPLatch launches |
| `fix_openssl` | `true` | Allow servers that need legacy OpenSSL compatibility |
| `ignore_tls_errors` | `false` | Ignore TLS certificate errors. Only use this if you trust the server. |

## Troubleshooting

If a connection fails, check `~/.config/gplatch/gplatch.log` for the details.

## Permissions

A VPN needs root access to set up its network connection. GPLatch keeps that as small
as it can:

- The app and the login window run as your normal user.
- Only `gpclient` runs as root, started through `pkexec`.
- The included polkit rule lets `wheel` users in an active local session run only the
  connect and disconnect commands GPLatch needs, without a password. Anything else
  gets the normal password prompt.

## License

GPLatch is licensed under the [GNU General Public License v3.0](LICENSE).

It is an independent project, not affiliated with Palo Alto Networks or GPGUI.
GlobalProtect is a trademark of Palo Alto Networks.
