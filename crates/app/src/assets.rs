//! Icons (Tabler, MIT) embedded into the binary and served to gpui's SVG renderer.

use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../../../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icons!(
    "users",
    "broadcast",
    "arrows-exchange",
    "settings",
    "dots",
    "copy",
    "eye",
    "eye-off",
    "plus",
    "pick",
    "device-gamepad-2",
    "bell",
    "check",
    "refresh",
    "trash",
    "pencil",
    "x",
    "chevron-down",
    "link",
    "user-plus",
);

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

/// Icon path for [`gpui::svg`].
pub fn icon(name: &str) -> SharedString {
    SharedString::from(format!("icons/{name}.svg"))
}
