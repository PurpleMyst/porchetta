use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use owo_colors::Style;

static QUIET: AtomicBool = AtomicBool::new(false);
// 0 = auto, 1 = always, 2 = never.
static COLOR: AtomicU8 = AtomicU8::new(0);

/// Configure human-readable output. Command data remains on stdout.
pub fn configure(quiet: bool, color: &str) {
    QUIET.store(quiet, Ordering::Relaxed);
    COLOR.store(
        match color {
            "always" => 1,
            "never" => 2,
            _ => 0,
        },
        Ordering::Relaxed,
    );
}

fn colors_enabled() -> bool {
    match COLOR.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            std::io::stderr().is_terminal()
                && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
                && std::env::var_os("TERM").is_none_or(|value| value != "dumb")
        }
    }
}

fn styled(text: &str, style: Style) -> ColoredText<'_> {
    ColoredText { text, style }
}

fn routine(text: impl std::fmt::Display) {
    if !QUIET.load(Ordering::Relaxed) {
        eprintln!("{text}");
    }
}

/// Print a section header.
pub fn header(text: &str) {
    routine(format_args!("\n{}", styled(text, Style::new().bold())));
}

/// Introduce a sync or preview and its mutation guarantees.
pub fn sync_header(hostname: &str, dry_run: bool, offline: bool, remotes: usize) {
    header(&format!("Porchetta · {hostname}"));
    let mode = if offline { "offline" } else { "online" };
    info(&format!(
        "{} · {mode} · {remotes} remotes",
        if dry_run { "Preview" } else { "Sync" }
    ));
    if dry_run {
        info("No managed files or canonical store refs will change.");
        if !offline {
            muted("Remotes fetched; remote-tracking refs may be refreshed.");
        }
    }
}

/// Print a success message.
pub fn success(text: &str) {
    routine(format_args!(
        "{} {text}",
        styled("✓", Style::new().green().bold())
    ));
}

/// Print an error message, even in quiet mode.
pub fn error(text: &str) {
    eprintln!("{} {text}", styled("✗", Style::new().red().bold()));
}

/// Print a warning, even in quiet mode.
pub fn warning(text: &str) {
    eprintln!("{} {text}", styled("!", Style::new().yellow().bold()));
}

/// Print an informational message.
pub fn info(text: &str) {
    routine(text);
}

/// Print dimmed secondary text.
pub fn muted(text: &str) {
    routine(styled(text, Style::new().dimmed()));
}

/// Print a bullet point.
pub fn bullet(text: &str) {
    routine(format_args!(
        "  {} {text}",
        styled("●", Style::new().cyan())
    ));
}

/// Print a manifest content block.
pub fn manifest_block(content: &str) {
    for line in content.lines() {
        muted(&format!("  {line}"));
    }
}

/// Print a topic row. Conflicts remain visible in quiet mode.
pub fn topic(name: &str, status: &str, detail: &str) {
    let symbol = match status {
        "captured" | "would capture" => "↑",
        "applied" | "would apply" => "↓",
        "captured and applied" | "would capture and apply" => "↕",
        "conflict" => "!",
        _ => "·",
    };
    let name = format!("{name:<10}");
    let row = format_args!(
        "  {} {}  {}",
        color_status_for(symbol, status),
        styled(&name, Style::new().bold()),
        color_status_for(detail, status)
    );
    if status == "conflict" {
        eprintln!("{row}");
    } else {
        routine(row);
    }
}

/// Print an individual file change. Paths stay readable; deletions stand out.
pub fn file_change(destination: &str, action: &str, path: &str) {
    let destination_style = if destination == "Store" {
        Style::new().blue()
    } else {
        Style::new().cyan()
    };
    let action_style = match action {
        "delete" => Style::new().yellow().bold(),
        "add" => Style::new().green(),
        _ => Style::new(),
    };
    let destination = format!("{destination:<7}");
    let action = format!("{action:<6}");
    routine(format_args!(
        "      {}  {} {path}",
        styled(&destination, destination_style),
        styled(&action, action_style)
    ));
}

/// Print a remote's publication result.
pub fn publication(name: &str, failure: Option<&str>) {
    if let Some(reason) = failure {
        eprintln!(
            "  {} {}  {reason}",
            styled("!", Style::new().yellow().bold()),
            styled(name, Style::new().bold())
        );
    } else {
        routine(format_args!(
            "  {} {}  Published",
            styled("✓", Style::new().green()),
            styled(name, Style::new().bold())
        ));
    }
}

/// Color a sync status string for display.
pub fn color_status(text: &str) -> ColoredText<'_> {
    color_status_for(text, text)
}

fn color_status_for<'a>(text: &'a str, status: &str) -> ColoredText<'a> {
    let style = match status {
        "unchanged" => Style::new().dimmed(),
        "captured" | "would capture" => Style::new().blue(),
        "applied" | "would apply" => Style::new().cyan(),
        "captured and applied" | "would capture and apply" => Style::new().blue().bold(),
        "conflict" => Style::new().red().bold(),
        _ => Style::new(),
    };
    styled(text, style)
}

/// A non-allocating wrapper that styles text only when color is enabled.
#[must_use]
pub struct ColoredText<'a> {
    text: &'a str,
    style: Style,
}

impl std::fmt::Display for ColoredText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if colors_enabled() {
            self.style.fmt_prefix(f)?;
        }
        f.write_str(self.text)?;
        if colors_enabled() {
            self.style.fmt_suffix(f)?;
        }
        Ok(())
    }
}
