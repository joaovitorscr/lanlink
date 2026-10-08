//! Colors and user preferences for appearance. The active [`Theme`] is a gpui global so
//! every widget reads the same palette; it is recomputed on each root render from the
//! saved preference and the window's system appearance.

use gpui::{rgba, Global, Hsla, WindowAppearance};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What the user picked in Settings → Appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

/// App-only preferences (not part of the core config): `<config dir>/app.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub appearance: Appearance,
    /// Translucent window with blur / glass behind it. Off = opaque.
    #[serde(default = "default_true")]
    pub transparency: bool,
    /// Closing the window hides it to the tray / menu bar and keeps the node running.
    #[serde(default = "default_true")]
    pub keep_in_background: bool,
    /// Start lanlink, hidden, when the user logs in.
    #[serde(default)]
    pub launch_at_login: bool,
}

fn default_true() -> bool {
    true
}

impl Prefs {
    fn path() -> PathBuf {
        lanlink_core::Config::dir().join("app.json")
    }

    pub fn load() -> Prefs {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Prefs {
                appearance: Appearance::System,
                transparency: true,
                keep_in_background: true,
                launch_at_login: false,
            })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Window content surface (translucent when `transparent`).
    pub content: Hsla,
    /// Floating sidebar panel.
    pub sidebar: Hsla,
    pub sidebar_stroke: Hsla,
    pub nav_sel: Hsla,
    pub fg: Hsla,
    pub fg2: Hsla,
    pub fg3: Hsla,
    pub card: Hsla,
    pub card_stroke: Hsla,
    pub sep: Hsla,
    pub field: Hsla,
    pub btn: Hsla,
    pub btn_stroke: Hsla,
    pub hover: Hsla,
    pub menu: Hsla,
    pub sheet: Hsla,
    pub scrim: Hsla,
    pub blue: Hsla,
    pub green: Hsla,
    pub orange: Hsla,
    pub red: Hsla,
    pub purple: Hsla,
    pub teal: Hsla,
    pub white: Hsla,
}

impl Global for Theme {}

fn c(v: u32) -> Hsla {
    rgba(v).into()
}

impl Theme {
    pub fn resolve(prefs: &Prefs, system: WindowAppearance) -> Theme {
        let dark = match prefs.appearance {
            Appearance::Dark => true,
            Appearance::Light => false,
            Appearance::System => matches!(
                system,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        };
        Theme::new(dark, prefs.transparency)
    }

    pub fn new(dark: bool, transparent: bool) -> Theme {
        if dark {
            Theme {
                content: if transparent {
                    c(0x1e1e21d1)
                } else {
                    c(0x1e1e21ff)
                },
                sidebar: if transparent {
                    c(0x3c3c4661)
                } else {
                    c(0x242428ff)
                },
                sidebar_stroke: c(0xffffff38),
                nav_sel: c(0xffffff29),
                fg: c(0xf5f5f7ff),
                fg2: c(0x98989dff),
                fg3: c(0x636366ff),
                card: c(0xffffff0e),
                card_stroke: c(0xffffff17),
                sep: c(0xffffff14),
                field: c(0xffffff12),
                btn: c(0xffffff1a),
                btn_stroke: c(0xffffff24),
                hover: c(0xffffff0f),
                menu: c(0x2c2c30f0),
                sheet: c(0x28282cfa),
                scrim: c(0x00000061),
                blue: c(0x0a84ffff),
                green: c(0x30d158ff),
                orange: c(0xff9f0aff),
                red: c(0xff453aff),
                purple: c(0xbf5af2ff),
                teal: c(0x64d2ffff),
                white: c(0xffffffff),
            }
        } else {
            Theme {
                content: if transparent {
                    c(0xf6f6f8db)
                } else {
                    c(0xf6f6f8ff)
                },
                sidebar: if transparent {
                    c(0xffffff6b)
                } else {
                    c(0xececf1ff)
                },
                sidebar_stroke: c(0xffffffb3),
                nav_sel: c(0xffffffbf),
                fg: c(0x1d1d1fff),
                fg2: c(0x6e6e73ff),
                fg3: c(0xaeaeb2ff),
                card: c(0xffffffc7),
                card_stroke: c(0x0000001a),
                sep: c(0x00000014),
                field: c(0x0000000d),
                btn: c(0xffffffe6),
                btn_stroke: c(0x00000029),
                hover: c(0x0000000d),
                menu: c(0xf6f6f8f2),
                sheet: c(0xf6f6f8fa),
                scrim: c(0x00000047),
                blue: c(0x007affff),
                green: c(0x28a745ff),
                orange: c(0xc67600ff),
                red: c(0xff3b30ff),
                purple: c(0xaf52deff),
                teal: c(0x0a84ffff),
                white: c(0xffffffff),
            }
        }
    }

    /// Avatar background for a peer id: stable, from a small palette.
    pub fn avatar_color(&self, key: &str) -> Hsla {
        let palette = [
            self.orange,
            self.teal,
            self.purple,
            self.green,
            c(0xff375fff),
            c(0x5e5ce6ff),
        ];
        let h = key
            .bytes()
            .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        palette[(h % palette.len() as u32) as usize]
    }
}
