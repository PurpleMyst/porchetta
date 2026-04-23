use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use log::{debug, info, trace, warn};

use crate::manifest::Manifest;
use crate::store::PorchettaStore;
use crate::ui;

// ─── Attribute parsing ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileKind {
    Regular,
    Create,
    Script,
    Symlink,
    Modify,
    Remove,
}

#[derive(Debug, Clone)]
#[allow(dead_code, clippy::struct_excessive_bools)]
struct DirAttr {
    target_name: String,
    exact: bool,
    external: bool,
    private: bool,
    read_only: bool,
    remove: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code, clippy::struct_excessive_bools)]
struct FileAttr {
    target_name: String,
    kind: FileKind,
    template: bool,
    encrypted: bool,
    executable: bool,
    private: bool,
    read_only: bool,
    empty: bool,
}

fn parse_dir_name(name: &str) -> Result<DirAttr> {
    if name.is_empty() {
        bail!("empty directory name");
    }
    let original = name;
    let mut name = name;

    let (remove, stripped) = strip_prefix(name, "remove_");
    name = stripped;
    let (external, stripped) = strip_prefix(name, "external_");
    name = stripped;
    let (exact, stripped) = strip_prefix(name, "exact_");
    name = stripped;
    let (private, stripped) = strip_prefix(name, "private_");
    name = stripped;
    let (read_only, stripped) = strip_prefix(name, "readonly_");
    name = stripped;

    let name_prefix: &str;
    if let Some(rest) = name.strip_prefix("dot_") {
        name_prefix = ".";
        name = rest;
    } else if let Some(rest) = name.strip_prefix("literal_") {
        name_prefix = "";
        name = rest;
    } else {
        name_prefix = "";
    }

    if name.is_empty() {
        bail!("{original}: invalid directory name");
    }

    Ok(DirAttr {
        target_name: format!("{name_prefix}{name}"),
        exact,
        external,
        private,
        read_only,
        remove,
    })
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn parse_file_name(name: &str) -> Result<FileAttr> {
    if name.is_empty() {
        bail!("empty filename");
    }
    let original = name;
    let mut name = name;

    let kind: FileKind;

    if let Some(rest) = name.strip_prefix("create_") {
        kind = FileKind::Create;
        name = rest;
        name = strip_encrypted(name);
        name = strip_private(name);
        name = strip_readonly(name);
        name = strip_empty(name);
        name = strip_executable(name);
    } else if name.starts_with("remove_") {
        kind = FileKind::Remove;
        name = &name["remove_".len()..];
    } else if name.starts_with("run_") {
        kind = FileKind::Script;
        name = &name["run_".len()..];
        // skip once_/onchange_/before_/after_ — all scripts are skipped anyway
        name = strip_script_condition(name);
        name = strip_script_order(name);
    } else if name.starts_with("symlink_") {
        kind = FileKind::Symlink;
        name = &name["symlink_".len()..];
    } else if let Some(rest) = name.strip_prefix("modify_") {
        kind = FileKind::Modify;
        name = rest;
        name = strip_encrypted(name);
        name = strip_private(name);
        name = strip_readonly(name);
        name = strip_executable(name);
    } else {
        kind = FileKind::Regular;
        name = strip_encrypted(name);
        name = strip_private(name);
        name = strip_readonly(name);
        name = strip_empty(name);
        name = strip_executable(name);
    }

    let name_prefix: &str;
    if let Some(rest) = name.strip_prefix("dot_") {
        name_prefix = ".";
        name = rest;
    } else if let Some(rest) = name.strip_prefix("literal_") {
        name_prefix = "";
        name = rest;
    } else {
        name_prefix = "";
    }

    // strip encrypted suffix if encrypted (chezmoi uses the encrypted tool suffix, e.g. .age)
    let encrypted = original.contains("encrypted_");
    if encrypted {
        // Strip any trailing suffix that looks like an encryption extension
        // chezmoi uses the encryption tool name as suffix (e.g. .age, .asc)
        if let Some(dot) = name.rfind('.') {
            name = &name[..dot];
        }
    }

    let template: bool;
    if name.ends_with(".literal") {
        name = &name[..name.len() - ".literal".len()];
        template = false;
    } else if name.ends_with(".tmpl") {
        name = &name[..name.len() - ".tmpl".len()];
        template = true;
        // .tmpl may follow .literal, but chezmoi doesn't allow both; .literal inside .tmpl is
        // handled by stripping .tmpl first.
        if name.ends_with(".literal") {
            name = &name[..name.len() - ".literal".len()];
        }
    } else {
        template = false;
    }

    if name.is_empty() {
        bail!("{original}: invalid filename");
    }

    Ok(FileAttr {
        target_name: format!("{name_prefix}{name}"),
        kind,
        template,
        encrypted,
        executable: original.contains("executable_"),
        private: original.contains("private_"),
        read_only: original.contains("readonly_"),
        empty: original.contains("empty_"),
    })
}

// ─── helper strip functions ──────────────────────────────────────────────────

#[allow(clippy::needless_lifetimes)]
fn strip_prefix<'a>(s: &'a str, prefix: &str) -> (bool, &'a str) {
    if let Some(rest) = s.strip_prefix(prefix) {
        (true, rest)
    } else {
        (false, s)
    }
}

fn strip_encrypted(name: &str) -> &str {
    name.strip_prefix("encrypted_").unwrap_or(name)
}

fn strip_private(name: &str) -> &str {
    name.strip_prefix("private_").unwrap_or(name)
}

fn strip_readonly(name: &str) -> &str {
    name.strip_prefix("readonly_").unwrap_or(name)
}

fn strip_empty(name: &str) -> &str {
    name.strip_prefix("empty_").unwrap_or(name)
}

fn strip_executable(name: &str) -> &str {
    name.strip_prefix("executable_").unwrap_or(name)
}

fn strip_script_condition(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix("once_") {
        return rest;
    }
    if let Some(rest) = name.strip_prefix("onchange_") {
        return rest;
    }
    name
}

fn strip_script_order(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix("before_") {
        return rest;
    }
    if let Some(rest) = name.strip_prefix("after_") {
        return rest;
    }
    name
}

// ─── Special chezmoi files / dirs to skip ────────────────────────────────────

fn is_special_chezmoi_file(name: &str) -> bool {
    if !name.starts_with(".chezmoi") {
        return false;
    }
    let known = [
        ".chezmoiignore",
        ".chezmoiremove",
        ".chezmoiversion",
        ".chezmoiroot",
        ".chezmoi.json.tmpl",
        ".chezmoi.toml.tmpl",
        ".chezmoi.yaml.tmpl",
        ".chezmoidata.json",
        ".chezmoidata.toml",
        ".chezmoidata.yaml",
        ".chezmoiexternal.json",
        ".chezmoiexternal.toml",
        ".chezmoiexternal.yaml",
        ".chezmoiexternal.json.tmpl",
        ".chezmoiexternal.toml.tmpl",
        ".chezmoiexternal.yaml.tmpl",
        ".chezmoiignore.tmpl",
        ".chezmoiremove.tmpl",
    ];
    known.contains(&name)
}

fn is_special_chezmoi_dir(name: &str) -> bool {
    let known = [
        ".chezmoitemplates",
        ".chezmoiscripts",
        ".chezmoidata",
        ".chezmoiexternals",
    ];
    known.contains(&name)
}

// ─── Target entry ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct TargetEntry {
    target_rel_path: PathBuf,
    kind: FileKind,
    template: bool,
    encrypted: bool,
}

// ─── Target path remapping ─────────────────────────────────────────────────

/// Remaps Windows `AppData/Local` and `AppData/Roaming` prefixes to `.config`.
fn remap_target_path(path: &Path) -> PathBuf {
    let components: Vec<&str> = path
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or(""))
        .collect();
    if components.len() >= 2
        && components[0] == "AppData"
        && (components[1] == "Local" || components[1] == "Roaming")
    {
        let rest: PathBuf = components.iter().skip(2).collect();
        return PathBuf::from(".config").join(rest);
    }
    path.to_path_buf()
}

// ─── Walker ──────────────────────────────────────────────────────────────────

fn walk_source_dir(
    source_root: &Path,
    rel_dir: &Path,
    target_prefix: &Path,
    entries: &mut Vec<TargetEntry>,
    warnings: &mut Warnings,
) -> Result<()> {
    let abs_dir = source_root.join(rel_dir);
    let dir_reader = std::fs::read_dir(&abs_dir)
        .with_context(|| format!("failed to read dir {}", abs_dir.display()))?;

    for entry in dir_reader {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let file_type = entry.file_type()?;

        if file_type.is_dir() && name == ".git" {
            trace!("skipping .git directory");
            continue;
        }

        if name.starts_with(".chezmoi") {
            if file_type.is_dir() && is_special_chezmoi_dir(&name) {
                trace!("skipping special chezmoi dir: {name}");
                continue;
            }
            if file_type.is_file() && is_special_chezmoi_file(&name) {
                trace!("skipping special chezmoi file: {name}");
                continue;
            }
            if name.starts_with(".chezmoi") {
                trace!("skipping unknown .chezmoi entry: {name}");
                continue;
            }
        }

        if file_type.is_dir() {
            let dir_attr = match parse_dir_name(&name) {
                Ok(a) => a,
                Err(e) => {
                    warn!("skipping directory '{name}': {e}");
                    continue;
                }
            };

            if dir_attr.remove {
                trace!("skipping remove_ directory: {name}");
                continue;
            }
            if dir_attr.external {
                warnings.external_dirs += 1;
                trace!("skipping external_ directory: {name}");
                continue;
            }

            let new_target = remap_target_path(&target_prefix.join(&dir_attr.target_name));
            let new_rel = rel_dir.join(name.as_ref());
            walk_source_dir(source_root, &new_rel, &new_target, entries, warnings)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            let file_attr = match parse_file_name(&name) {
                Ok(a) => a,
                Err(e) => {
                    warn!("skipping file '{name}': {e}");
                    continue;
                }
            };

            match file_attr.kind {
                FileKind::Remove => {
                    trace!("skipping remove_ file: {name}");
                    continue;
                }
                FileKind::Script => {
                    warnings.scripts += 1;
                    trace!("skipping script: {name}");
                    continue;
                }
                FileKind::Symlink => {
                    warnings.symlinks += 1;
                    trace!("skipping symlink: {name}");
                    continue;
                }
                FileKind::Modify => {
                    warnings.modifies += 1;
                    trace!("skipping modify script: {name}");
                    continue;
                }
                FileKind::Create => {
                    warnings.creates += 1;
                }
                FileKind::Regular => {}
            }

            if file_attr.template {
                warnings.templates += 1;
            }
            if file_attr.encrypted {
                warnings.encrypted += 1;
            }

            let target_path = remap_target_path(&target_prefix.join(&file_attr.target_name));
            entries.push(TargetEntry {
                target_rel_path: target_path,
                kind: file_attr.kind,
                template: file_attr.template,
                encrypted: file_attr.encrypted,
            });
        }
    }

    Ok(())
}

#[derive(Debug, Default)]
struct Warnings {
    scripts: usize,
    symlinks: usize,
    modifies: usize,
    creates: usize,
    templates: usize,
    encrypted: usize,
    external_dirs: usize,
}

impl Warnings {
    fn has_any(&self) -> bool {
        self.scripts > 0
            || self.symlinks > 0
            || self.modifies > 0
            || self.creates > 0
            || self.templates > 0
            || self.encrypted > 0
            || self.external_dirs > 0
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.scripts > 0 {
            parts.push(format!("{} script(s) skipped", self.scripts));
        }
        if self.symlinks > 0 {
            parts.push(format!("{} symlink(s) skipped", self.symlinks));
        }
        if self.modifies > 0 {
            parts.push(format!("{} modify script(s) skipped", self.modifies));
        }
        if self.creates > 0 {
            parts.push(format!("{} create-only file(s) (semantics lost)", self.creates));
        }
        if self.templates > 0 {
            parts.push(format!("{} template(s) detected", self.templates));
        }
        if self.encrypted > 0 {
            parts.push(format!("{} encrypted file(s) detected", self.encrypted));
        }
        if self.external_dirs > 0 {
            parts.push(format!("{} external dir(s) skipped", self.external_dirs));
        }
        parts.join(", ")
    }
}

// ─── Manifest path computation ─────────────────────────────────────────────

/// Decides which paths appear in the manifest by collapsing directories that are
/// fully managed (every filesystem entry is either a managed file or a collapsed
/// subdirectory). Processes bottom-up so a parent's decision can reuse its
/// children's decisions.
fn compute_manifest_paths(
    managed_files: &HashSet<PathBuf>,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    // Collect all directories that appear as ancestors of managed files.
    let mut dirs: Vec<PathBuf> = Vec::new();
    for file in managed_files {
        let mut parent = file.parent();
        while let Some(p) = parent {
            if p.as_os_str().is_empty() {
                break;
            }
            dirs.push(p.to_path_buf());
            parent = p.parent();
        }
    }

    dirs.sort();
    dirs.dedup();
    // Deepest first so children are resolved before their parents.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));

    let mut collapsed: HashSet<PathBuf> = HashSet::new();

    for dir in &dirs {
        let abs_dir = home.join(dir);

        let all_managed = if abs_dir.is_dir() {
            let mut ok = true;
            for entry in std::fs::read_dir(&abs_dir)
                .with_context(|| format!("failed to read dir {}", abs_dir.display()))?
            {
                let entry = entry?;
                let entry_name = entry.file_name();
                let entry_rel = dir.join(&entry_name);

                let is_managed = if entry.file_type()?.is_dir() {
                    collapsed.contains(&entry_rel)
                } else {
                    managed_files.contains(&entry_rel)
                };

                if !is_managed {
                    ok = false;
                    break;
                }
            }
            ok
        } else {
            // Directory does not exist on the filesystem yet: trivially collapsible
            // because there are no unmanaged files to accidentally capture.
            true
        };

        if all_managed {
            collapsed.insert(dir.clone());
        }
    }

    // Keep only maximal collapsed dirs (not children of other collapsed dirs).
    let mut maximal: Vec<PathBuf> = Vec::new();
    for dir in &collapsed {
        let is_child = collapsed
            .iter()
            .any(|other| dir != other && dir.starts_with(other));
        if !is_child {
            maximal.push(dir.clone());
        }
    }

    // Final manifest paths: maximal collapsed dirs plus individual files
    // that are not already covered by a collapsed ancestor.
    let mut paths: Vec<PathBuf> = maximal.clone();
    for file in managed_files {
        let inside_collapsed = maximal.iter().any(|m| file.starts_with(m));
        if !inside_collapsed {
            paths.push(file.clone());
        }
    }

    paths.sort();
    Ok(paths)
}

fn topic_name_for_path(path: &Path) -> String {
    let components: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let n = components.len();

    if n == 1 {
        let file = &components[0];
        if matches!(
            file.as_str(),
            ".bashrc"
                | ".bash_profile"
                | ".bash_logout"
                | ".zshrc"
                | ".zprofile"
                | ".zshenv"
                | ".zlogin"
                | ".zlogout"
                | ".profile"
                | ".inputrc"
        ) {
            return "shell".to_string();
        }
        if matches!(
            file.as_str(),
            ".gitconfig" | ".gitignore_global" | ".gitattributes_global"
        ) {
            return "git".to_string();
        }
        if file == ".tmux.conf" {
            return "tmux".to_string();
        }
        if matches!(file.as_str(), ".vimrc" | ".nvimrc" | ".gvimrc") {
            return "vim".to_string();
        }
        return "home".to_string();
    }

    if n >= 2 && components[0] == ".config" {
        return components[1].clone();
    }

    if n >= 1 && components[0] == ".ssh" {
        return "ssh".to_string();
    }

    if n >= 2 && components[0] == ".local" && components[1] == "bin" {
        return "bin".to_string();
    }

    if n >= 3 && components[0] == ".local" && components[1] == "share" {
        return components[2].clone();
    }

    if n == 2 {
        return components[0].clone();
    }

    components[1].clone()
}

// ─── Topic grouping ──────────────────────────────────────────────────────────

fn group_into_topics(
    entries: Vec<TargetEntry>,
    home: &Path,
) -> Result<HashMap<String, Vec<PathBuf>>> {
    let managed_files: HashSet<PathBuf> =
        entries.into_iter().map(|e| e.target_rel_path).collect();
    let manifest_paths = compute_manifest_paths(&managed_files, home)?;

    let mut topics: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut seen: HashSet<(String, PathBuf)> = HashSet::new();

    for path in manifest_paths {
        let topic = topic_name_for_path(&path);
        let key = (topic.clone(), path.clone());
        if seen.insert(key) {
            topics.entry(topic).or_default().push(path);
        }
    }

    for paths in topics.values_mut() {
        paths.sort();
    }
    Ok(topics)
}

// ─── Manifest serialization ──────────────────────────────────────────────────

fn generate_manifest(topics: &HashMap<String, Vec<PathBuf>>) -> Result<Vec<u8>> {
    let mut buf = String::new();
    buf.push_str("return {\n");
    buf.push_str("    topics = {\n");

    // Sort topic names for deterministic output
    let mut topic_names: Vec<&String> = topics.keys().collect();
    topic_names.sort();

    for topic in topic_names {
        let paths = &topics[topic];
        let _ = std::fmt::Write::write_fmt(
            &mut buf,
            format_args!("        {topic} = {{\n"),
        );
        buf.push_str("            paths = {");
        for (i, p) in paths.iter().enumerate() {
            let s = p.to_string_lossy().replace('\\', "/");
            if i == 0 {
                buf.push('"');
            } else {
                buf.push_str(", \"");
            }
            buf.push_str(&s);
            buf.push('"');
        }
        buf.push_str("}\n");
        buf.push_str("        },\n");
    }

    buf.push_str("    }\n");
    buf.push_str("}\n");

    let bytes = buf.into_bytes();

    // Validate by round-tripping through Manifest::load
    let _manifest = Manifest::load(&bytes)
        .context("generated manifest failed validation — this is a bug")?;

    Ok(bytes)
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Migrate a chezmoi source directory into a porchetta manifest.
///
/// Walks the chezmoi source tree, resolves target paths, groups them into topics,
/// generates a manifest.lua, and commits it to the store.
///
/// # Errors
///
/// Returns an error if the source directory cannot be read, if parsing fails,
/// or if writing the manifest to the store fails.
pub fn migrate(
    store: &PorchettaStore,
    source_dir: &Path,
    yes: bool,
) -> Result<()> {
    info!("Migrating chezmoi source directory: {}", source_dir.display());

    if !source_dir.is_dir() {
        bail!("source directory '{}' does not exist", source_dir.display());
    }

    let mut entries = Vec::new();
    let mut warnings = Warnings::default();

    walk_source_dir(source_dir, Path::new(""), Path::new(""), &mut entries, &mut warnings)?;

    let entry_count = entries.len();
    debug!("Resolved {entry_count} target entries from chezmoi source");

    let home = dirs::home_dir().context("Could not determine home directory")?;
    let topics = group_into_topics(entries, &home)?;
    let topic_count = topics.len();
    let path_count: usize = topics.values().map(Vec::len).sum();

    // ── Preview ──────────────────────────────────────────────────────────────

    ui::header("Migration preview");
    ui::info(&format!(
        "Migrated {entry_count} chezmoi entr{} into {path_count} manifest path{} in {topic_count} topic{}",
        if entry_count == 1 { "y" } else { "ies" },
        if path_count == 1 { "" } else { "s" },
        if topic_count == 1 { "" } else { "s" }
    ));

    let mut topic_names: Vec<&String> = topics.keys().collect();
    topic_names.sort();

    for topic in &topic_names {
        let paths = &topics[*topic];
        let path_list: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        ui::bullet(&format!("{}  ({})", topic, path_list.join(", ")));
    }

    if warnings.has_any() {
        ui::info(&format!("Warnings: {}", warnings.summary()));
    }

    // ── Confirmation ─────────────────────────────────────────────────────────

    if !yes {
        let confirmed = inquire::Confirm::new("Commit this manifest?")
            .with_default(true)
            .prompt()
            .context("user cancelled migration")?;
        if !confirmed {
            ui::info("Migration cancelled");
            return Ok(());
        }
    }

    // ── Write manifest ───────────────────────────────────────────────────────

    let manifest_bytes = generate_manifest(&topics)?;
    store.write_manifest(&manifest_bytes)?;
    ui::success("Manifest committed");

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dir_name() {
        let d = parse_dir_name("dot_config").unwrap();
        assert_eq!(d.target_name, ".config");
        assert!(!d.exact);
        assert!(!d.external);

        let d = parse_dir_name("exact_dot_ssh").unwrap();
        assert_eq!(d.target_name, ".ssh");
        assert!(d.exact);

        let d = parse_dir_name("exact_private_foo").unwrap();
        assert_eq!(d.target_name, "foo");
        assert!(d.exact);
        assert!(d.private);

        let d = parse_dir_name("literal_dot_").unwrap();
        assert_eq!(d.target_name, "dot_");
    }

    #[test]
    fn test_parse_file_name() {
        let f = parse_file_name("dot_bashrc").unwrap();
        assert_eq!(f.target_name, ".bashrc");
        assert_eq!(f.kind, FileKind::Regular);
        assert!(!f.template);

        let f = parse_file_name("dot_gitconfig.tmpl").unwrap();
        assert_eq!(f.target_name, ".gitconfig");
        assert!(f.template);

        let f = parse_file_name("private_executable_dot_zshrc").unwrap();
        assert_eq!(f.target_name, ".zshrc");
        assert!(f.private);
        assert!(f.executable);

        let f = parse_file_name("create_dot_foo").unwrap();
        assert_eq!(f.target_name, ".foo");
        assert_eq!(f.kind, FileKind::Create);

        let f = parse_file_name("encrypted_private_dot_secret.age").unwrap();
        assert_eq!(f.target_name, ".secret");
        assert!(f.encrypted);
        assert!(f.private);
    }

    #[test]
    fn test_parse_script_names() {
        let f = parse_file_name("run_once_install.sh").unwrap();
        assert_eq!(f.target_name, "install.sh");
        assert_eq!(f.kind, FileKind::Script);

        let f = parse_file_name("run_onchange_after_setup.sh").unwrap();
        assert_eq!(f.target_name, "setup.sh");
        assert_eq!(f.kind, FileKind::Script);
    }

    #[test]
    fn test_collapse_fully_managed_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("a/b")).unwrap();
        std::fs::write(home.join("a/b/c.txt"), "").unwrap();
        std::fs::write(home.join("a/b/d.txt"), "").unwrap();
        std::fs::write(home.join("a/x.txt"), "").unwrap(); // unmanaged sibling

        let managed: HashSet<PathBuf> =
            ["a/b/c.txt", "a/b/d.txt"].iter().map(PathBuf::from).collect();
        let paths = compute_manifest_paths(&managed, home).unwrap();
        assert_eq!(paths, vec![PathBuf::from("a/b")]);
    }

    #[test]
    fn test_no_collapse_with_unmanaged() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("a")).unwrap();
        std::fs::write(home.join("a/b.txt"), "").unwrap();
        std::fs::write(home.join("a/x.txt"), "").unwrap();

        let managed: HashSet<PathBuf> = ["a/b.txt"].iter().map(PathBuf::from).collect();
        let paths = compute_manifest_paths(&managed, home).unwrap();
        assert_eq!(paths, vec![PathBuf::from("a/b.txt")]);
    }

    #[test]
    fn test_partial_collapse() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("a/b")).unwrap();
        std::fs::write(home.join("a/b/c.txt"), "").unwrap();
        std::fs::write(home.join("a/b/d.txt"), "").unwrap();
        std::fs::write(home.join("a/e.txt"), "").unwrap();
        std::fs::write(home.join("a/f.txt"), "").unwrap(); // unmanaged

        let managed: HashSet<PathBuf> = ["a/b/c.txt", "a/b/d.txt", "a/e.txt"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let paths = compute_manifest_paths(&managed, home).unwrap();
        assert!(paths.contains(&PathBuf::from("a/b")));
        assert!(paths.contains(&PathBuf::from("a/e.txt")));
        assert!(!paths.contains(&PathBuf::from("a/b/c.txt")));
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_group_into_topics() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Set up filesystem matching the managed files
        std::fs::create_dir_all(home.join(".config/nvim/lua")).unwrap();
        std::fs::write(home.join(".config/nvim/init.lua"), "").unwrap();
        std::fs::write(home.join(".config/nvim/lua/plugins.lua"), "").unwrap();
        std::fs::create_dir_all(home.join(".config/other_app")).unwrap(); // unmanaged
        std::fs::write(home.join(".bashrc"), "").unwrap();
        std::fs::write(home.join(".zshrc"), "").unwrap();
        std::fs::write(home.join(".gitconfig"), "").unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(home.join(".ssh/config"), "").unwrap();
        std::fs::write(home.join(".ssh/id_rsa"), "").unwrap(); // unmanaged
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::write(home.join(".local/bin/my-script"), "").unwrap();
        std::fs::create_dir_all(home.join(".local/share")).unwrap(); // unmanaged
        std::fs::write(home.join(".inputrc"), "").unwrap();

        let entries = vec![
            TargetEntry {
                target_rel_path: PathBuf::from(".config/nvim/init.lua"),
                kind: FileKind::Regular,
                template: false,
                encrypted: false,
            },
            TargetEntry {
                target_rel_path: PathBuf::from(".config/nvim/lua/plugins.lua"),
                kind: FileKind::Regular,
                template: false,
                encrypted: false,
            },
            TargetEntry {
                target_rel_path: PathBuf::from(".bashrc"),
                kind: FileKind::Regular,
                template: false,
                encrypted: false,
            },
            TargetEntry {
                target_rel_path: PathBuf::from(".zshrc"),
                kind: FileKind::Regular,
                template: false,
                encrypted: false,
            },
            TargetEntry {
                target_rel_path: PathBuf::from(".gitconfig"),
                kind: FileKind::Regular,
                template: false,
                encrypted: false,
            },
            TargetEntry {
                target_rel_path: PathBuf::from(".ssh/config"),
                kind: FileKind::Regular,
                template: false,
                encrypted: false,
            },
            TargetEntry {
                target_rel_path: PathBuf::from(".local/bin/my-script"),
                kind: FileKind::Regular,
                template: false,
                encrypted: false,
            },
            TargetEntry {
                target_rel_path: PathBuf::from(".inputrc"),
                kind: FileKind::Regular,
                template: false,
                encrypted: false,
            },
        ];

        let topics = group_into_topics(entries, home).unwrap();
        assert_eq!(topics.get("nvim"), Some(&vec![PathBuf::from(".config/nvim")]));
        assert!(topics.get("shell").unwrap().contains(&PathBuf::from(".bashrc")));
        assert!(topics.get("shell").unwrap().contains(&PathBuf::from(".zshrc")));
        assert!(topics.get("shell").unwrap().contains(&PathBuf::from(".inputrc")));
        assert_eq!(topics.get("git"), Some(&vec![PathBuf::from(".gitconfig")]));
        assert_eq!(topics.get("ssh"), Some(&vec![PathBuf::from(".ssh/config")]));
        assert_eq!(topics.get("bin"), Some(&vec![PathBuf::from(".local/bin")]));
    }

    #[test]
    fn test_generate_manifest_roundtrip() {
        let mut topics = HashMap::new();
        topics.insert(
            "shell".to_string(),
            vec![PathBuf::from(".bashrc"), PathBuf::from(".zshrc")],
        );
        topics.insert("nvim".to_string(), vec![PathBuf::from(".config/nvim")]);

        let bytes = generate_manifest(&topics).unwrap();
        let manifest = Manifest::load(&bytes).unwrap();
        assert_eq!(manifest.topics.len(), 2);
        assert_eq!(manifest.topics["shell"].paths.len(), 2);
        assert_eq!(manifest.topics["nvim"].paths.len(), 1);
    }
}
