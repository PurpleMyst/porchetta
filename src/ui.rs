use owo_colors::OwoColorize;

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
#[must_use]
pub fn status(text: &str) -> String {
    match text {
        "unchanged" => text.dimmed().to_string(),
        "captured" => text.blue().bold().to_string(),
        "applied" => text.yellow().bold().to_string(),
        "captured and applied" => text.green().bold().to_string(),
        "would capture" => text.blue().to_string(),
        "would apply" => text.yellow().to_string(),
        "would capture and apply" => text.green().to_string(),
        _ => text.to_string(),
    }
}
