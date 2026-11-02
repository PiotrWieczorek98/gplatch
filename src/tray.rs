//! Presents the connection state in the system tray and reports menu actions.

use std::sync::mpsc::Sender;

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, Icon, ToolTip, Tray};

use crate::config::config_dir;
use crate::vpn::State;
use crate::{APP_NAME, CONFIG_FILE, LOG_FILE, open};

const ICON_SIZE: i32 = 64;

pub enum Cmd {
    Toggle,
    Quit,
}

pub struct VpnTray {
    pub state: State,
    pub can_connect: bool,
    /// Details of the current connection, shown under the status.
    pub info: Vec<String>,
    pub tx: Sender<Cmd>,
}

impl VpnTray {
    fn status(&self) -> &'static str {
        match self.state {
            State::Off => "Off",
            State::Connecting => "Connecting...",
            State::Connected => "Connected",
        }
    }
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

    // Icons are sent as pixel data because a name lookup can fail until the desktop
    // refreshes its icon cache, which leaves the tray entry blank after installation.
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
            .for_each(|pixel| pixel.rotate_right(1)); // The StatusNotifierItem protocol requires ARGB.
        vec![Icon {
            width: ICON_SIZE,
            height: ICON_SIZE,
            data,
        }]
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: format!("GPLatch — {}", self.status()),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let label = match self.state {
            State::Off => "Connect",
            State::Connecting => "Cancel",
            State::Connected => "Disconnect",
        };
        let status = format!("Status: {}", self.status());
        let mut items: Vec<MenuItem<Self>> = std::iter::once(status)
            .chain(self.info.iter().cloned())
            .map(|label| {
                StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.extend([
            MenuItem::Separator,
            StandardItem {
                label: label.into(),
                enabled: self.can_connect || self.state != State::Off,
                activate: Box::new(|tray: &mut Self| drop(tray.tx.send(Cmd::Toggle))),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Open config".into(),
                activate: Box::new(|_: &mut Self| open(config_dir().join(CONFIG_FILE))),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open log".into(),
                activate: Box::new(|_: &mut Self| open(config_dir().join(LOG_FILE))),
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
        ]);
        items
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    #[test]
    fn icons_are_64px() {
        for state in [State::Off, State::Connecting, State::Connected] {
            let tray = VpnTray {
                state,
                can_connect: true,
                info: vec![],
                tx: channel().0,
            };
            assert_eq!(
                tray.icon_pixmap()[0].data.len(),
                (ICON_SIZE * ICON_SIZE * 4) as usize
            );
        }
    }
}
