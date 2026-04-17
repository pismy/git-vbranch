//! Terminal styling: ANSI colors + OSC 8 hyperlinks.
//!
//! All helpers degrade to plain text when color/hyperlinks are disabled, so
//! callers never need to branch on the mode themselves.

use std::io::IsTerminal;

/// Styling policy applied to user-facing output.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub color: bool,
    pub hyperlinks: bool,
}

impl Style {
    /// Decide whether to emit ANSI escapes.
    ///
    /// Disabled if any of:
    /// - `--no-color` was passed,
    /// - `NO_COLOR` env var is set (any value),
    /// - stdout is not a terminal.
    pub fn new(no_color_flag: bool) -> Self {
        let env_no_color = std::env::var_os("NO_COLOR").is_some();
        let tty = std::io::stdout().is_terminal();
        let enabled = !no_color_flag && !env_no_color && tty;
        Self {
            color: enabled,
            hyperlinks: enabled,
        }
    }

}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

pub const CYAN: &str = "\x1b[36m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const MAGENTA: &str = "\x1b[35m";

/// Wrap `text` in an ANSI color escape sequence if coloring is enabled.
pub fn color(style: Style, ansi: &str, text: &str) -> String {
    if style.color {
        format!("{ansi}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub fn bold(style: Style, text: &str) -> String {
    if style.color {
        format!("{BOLD}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub fn dim(style: Style, text: &str) -> String {
    if style.color {
        format!("{DIM}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Wrap `text` in an OSC 8 terminal hyperlink escape sequence. Falls back to
/// plain text when hyperlinks are disabled or `url` is empty.
pub fn hyperlink(style: Style, url: &str, text: &str) -> String {
    if style.hyperlinks && !url.is_empty() {
        format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
    } else {
        text.to_string()
    }
}
