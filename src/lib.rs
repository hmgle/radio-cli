mod artwork;
pub mod cli;
pub mod config;
pub mod directory;
pub mod engine;
pub mod error;
pub mod messages;
pub mod model;
pub mod player;
pub mod process;
pub mod recorder;
pub mod service;
pub mod store;
pub mod tui;

pub const USER_AGENT: &str = concat!("radio-cli/", env!("CARGO_PKG_VERSION"));

/// External text must never introduce terminal escape sequences or new lines.
pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(1024)
        .collect()
}

#[cfg(test)]
extern crate self as radio_cli;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
