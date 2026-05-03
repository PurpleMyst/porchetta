use owo_colors::{OwoColorize, Style};

/// Print a section header.
pub fn header(text: &str) {
    println!();
    println!("{}", text.cyan().bold().underline());
}

/// Print a success message.
pub fn success(text: &str) {
    println!("{} {}", "✓".green().bold(), text.bold());
}

/// Print an error message to stderr.
pub fn error(text: &str) {
    eprintln!("{} {}", "✗".red().bold(), text.red().bold());
}

/// Print an informational message.
pub fn info(text: &str) {
    println!("{} {}", "ℹ".blue().bold(), text);
}

/// Print dimmed / muted text.
pub fn muted(text: &str) {
    println!("{}", text.dimmed());
}

/// Print a bullet point.
pub fn bullet(text: &str) {
    println!("  {} {}", "●".cyan(), text);
}

/// Print a manifest content block with dimmed, indented formatting.
pub fn manifest_block(content: &str) {
    for line in content.lines() {
        println!("  {}", line.dimmed());
    }
}

/// Color a sync status string for display.
pub fn color_status(text: &str) -> ColoredText<'_> {
    const UNCHANGED: Style = Style::new().dimmed();
    const CAPTURED: Style = Style::new().blue().bold();
    const APPLIED: Style = Style::new().yellow().bold();
    const CAPTURED_AND_APPLIED: Style = Style::new().green().bold();
    const WOULD_CAPTURE: Style = Style::new().blue();
    const WOULD_APPLY: Style = Style::new().yellow();
    const WOULD_CAPTURE_AND_APPLY: Style = Style::new().green();
    const CONFLICT: Style = Style::new().red().bold();

    let style = match text {
        "unchanged" => UNCHANGED,
        "captured" => CAPTURED,
        "applied" => APPLIED,
        "captured and applied" => CAPTURED_AND_APPLIED,
        "would capture" => WOULD_CAPTURE,
        "would apply" => WOULD_APPLY,
        "would capture and apply" => WOULD_CAPTURE_AND_APPLY,
        "conflict" => CONFLICT,
        _other => Style::new(),
    };
    ColoredText { text, style }
}

/// A non-allocating wrapper that displays text with ANSI style.
#[must_use]
pub struct ColoredText<'a> {
    text: &'a str,
    style: Style,
}

impl std::fmt::Display for ColoredText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.style.fmt_prefix(f)?;
        f.write_str(self.text)?;
        self.style.fmt_suffix(f)
    }
}
