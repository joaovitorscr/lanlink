//! Which build this is. The release workflow sets LANLINK_VERSION, LANLINK_CHANNEL and
//! LANLINK_COMMIT at compile time; local builds fall back to the Cargo version and "dev".

/// Full version, e.g. "0.2.0" or "0.2.0-nightly.20261008.12".
pub const VERSION: &str = match option_env!("LANLINK_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// "stable", "nightly" or "dev".
pub const CHANNEL: &str = match option_env!("LANLINK_CHANNEL") {
    Some(c) => c,
    None => "dev",
};

/// Short commit hash, empty for local builds.
pub const COMMIT: &str = match option_env!("LANLINK_COMMIT") {
    Some(c) => c,
    None => "",
};

/// One line for About screens and `--version`, e.g. "0.2.0-nightly.20261008.12 (nightly, 1a2b3c4d5e6f)".
pub fn describe() -> String {
    if COMMIT.is_empty() {
        format!("{VERSION} ({CHANNEL})")
    } else {
        format!("{VERSION} ({CHANNEL}, {COMMIT})")
    }
}
