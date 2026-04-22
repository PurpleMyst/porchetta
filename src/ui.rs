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
