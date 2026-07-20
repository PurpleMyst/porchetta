use super::resolver::ConflictResolver;
use anyhow::{Context, Result, bail, ensure};
use gix::bstr::{BString, ByteSlice};
use gix::merge::tree::TreatAsUnresolved;
use gix::{ObjectId, diff::tree_with_rewrites::Change};
use log::{debug, warn};

/// Merges three trees with Porchetta's conflict labels (`name` is the topic or branch).
///
/// # Errors
///
/// Returns an error if the trees cannot be read or the merge fails.
pub fn merge_trees<'a>(
    repo: &'a gix::Repository,
    base: impl AsRef<gix::oid>,
    ours: impl AsRef<gix::oid>,
    theirs: impl AsRef<gix::oid>,
    name: &str,
) -> Result<gix::merge::tree::Outcome<'a>> {
    Ok(repo.merge_trees(
        base,
        ours,
        theirs,
        gix::merge::blob::builtin_driver::text::Labels {
            ancestor: Some(BString::from(format!("{name} (last applied)")).as_bstr()),
            current: Some(BString::from(format!("{name} (on system)")).as_bstr()),
            other: Some(BString::from(format!("{name} (in repo)")).as_bstr()),
        },
        repo.tree_merge_options()?,
    )?)
}

/// Merges two commits using their automatically determined merge base.
///
/// The returned outcome is the tree merge so callers can resolve conflicts and
/// write the resulting tree without retaining commit-merge bookkeeping.
///
/// # Errors
///
/// Returns an error if the merge base or commits cannot be read, or the merge fails.
pub fn merge_commits<'a>(
    repo: &'a gix::Repository,
    ours: impl Into<gix::ObjectId>,
    theirs: impl Into<gix::ObjectId>,
    name: &str,
) -> Result<gix::merge::tree::Outcome<'a>> {
    let ours_label = BString::from(format!("{name} (local)"));
    let theirs_label = BString::from(format!("{name} (remote)"));
    let outcome = repo.merge_commits(
        ours,
        theirs,
        gix::merge::blob::builtin_driver::text::Labels {
            ancestor: None,
            current: Some(ours_label.as_bstr()),
            other: Some(theirs_label.as_bstr()),
        },
        repo.tree_merge_options()?.into(),
    )?;
    Ok(outcome.tree_merge)
}

/// Tests whether `ancestor` is an ancestor of `descendant`.
///
/// # Errors
///
/// Returns an error if the object graph cannot be traversed.
pub fn is_ancestor(
    repo: &gix::Repository,
    ancestor: gix::ObjectId,
    descendant: gix::ObjectId,
) -> Result<bool> {
    match repo.merge_base(ancestor, descendant) {
        Ok(base) => Ok(base == ancestor),
        Err(gix::repository::merge_base::Error::NotFound { .. }) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Iterate over unresolved conflicts in `outcome` and resolve them using `resolver`.
///
/// # Errors
///
/// Returns an error if resolution fails or the user aborts.
pub fn resolve_conflicts(
    repo: &gix::Repository,
    resolver: &dyn ConflictResolver,
    outcome: &mut gix::merge::tree::Outcome<'_>,
) -> Result<()> {
    for conflict in &outcome.conflicts {
        if !conflict.is_unresolved(TreatAsUnresolved::default()) {
            continue;
        }
        resolve_conflict(repo, resolver, conflict, &mut outcome.tree)?;
    }
    Ok(())
}

/// Print a summary of all unresolved conflicts in the merge outcome.
pub fn show_conflicts(outcome: &gix::merge::tree::Outcome<'_>) {
    for conflict in &outcome.conflicts {
        if !conflict.is_unresolved(TreatAsUnresolved::default()) {
            continue;
        }
        let (ours_change, theirs_change) = conflict.changes_in_resolution();
        let location_description = conflict_location_description(ours_change, theirs_change);
        crate::ui::info(&format!(
            "  {location_description} changed both locally and in repo"
        ));
    }
}

fn resolve_conflict(
    repo: &gix::Repository,
    resolver: &dyn ConflictResolver,
    conflict: &gix::merge::tree::Conflict,
    merged_tree: &mut gix::object::tree::Editor<'_>,
) -> Result<()> {
    let (ours_change, theirs_change) = conflict.changes_in_resolution();
    let location_description = conflict_location_description(ours_change, theirs_change);

    warn!("Encountered unresolved conflict at {location_description}");
    debug!("Conflict resolution failure: {:?}", conflict.resolution);
    debug!("Our change: {ours_change:?}");
    debug!("Their change: {theirs_change:?}");

    if is_blob_level_conflict(conflict, ours_change, theirs_change) {
        resolve_blob_level_conflict(repo, resolver, conflict, merged_tree)?;
    } else {
        resolve_tree_level_conflict(resolver, conflict, merged_tree)?;
    }

    Ok(())
}

fn resolve_blob_level_conflict(
    repo: &gix::Repository,
    resolver: &dyn ConflictResolver,
    conflict: &gix::merge::tree::Conflict,
    merged_tree: &mut gix::object::tree::Editor<'_>,
) -> Result<()> {
    let (ours_change, theirs_change) = conflict.changes_in_resolution();
    ensure!(
        ours_change.location() == theirs_change.location(),
        "Blob-level conflict unexpectedly changed location from '{}' to '{}'",
        ours_change.location().to_str_lossy(),
        theirs_change.location().to_str_lossy()
    );

    let content_merge = conflict
        .content_merge()
        .context("Expected merged blob for blob-level conflict")?;
    let edited_blob_id = edit_blob_in_editor(
        repo,
        resolver,
        content_merge.merged_blob_id,
        &ours_change.location().to_str_lossy(),
    )?;

    let entry_kind = entry_kind_for_shared_location(resolver, ours_change, theirs_change)?;
    merged_tree.upsert(ours_change.location(), entry_kind, edited_blob_id)?;
    Ok(())
}

fn resolve_tree_level_conflict(
    resolver: &dyn ConflictResolver,
    conflict: &gix::merge::tree::Conflict,
    merged_tree: &mut gix::object::tree::Editor<'_>,
) -> Result<()> {
    let (ours_change, theirs_change) = conflict.changes_in_resolution();
    let prompt = tree_conflict_prompt(conflict, ours_change, theirs_change);
    match resolver.resolve_tree_conflict(&prompt)? {
        super::resolver::TreeConflictResolution::KeepOurs => {
            remove_change_effect_from_tree(merged_tree, theirs_change)?;
            apply_change_to_tree(merged_tree, ours_change)?;
        }
        super::resolver::TreeConflictResolution::KeepTheirs => {
            remove_change_effect_from_tree(merged_tree, ours_change)?;
            apply_change_to_tree(merged_tree, theirs_change)?;
        }
        super::resolver::TreeConflictResolution::Abort => {
            bail!("Sync aborted by user while resolving conflict");
        }
    }
    Ok(())
}

fn tree_conflict_prompt(
    conflict: &gix::merge::tree::Conflict,
    ours_change: &Change,
    theirs_change: &Change,
) -> String {
    let location_description = conflict_location_description(ours_change, theirs_change);
    let resolution_description = match &conflict.resolution {
        Ok(resolution) => format!("Resolution: {resolution:?}"),
        Err(failure) => format!("Unresolved reason: {failure:?}"),
    };

    let mut prompt = format!(
        "Resolve tree conflict at {location_description}\n\n{resolution_description}\n\nOur change: {}\nTheir change: {}",
        describe_change(ours_change),
        describe_change(theirs_change),
    );

    if conflict.content_merge().is_some() {
        prompt.push_str(
            "\n\nA merged blob exists, but this conflict still needs a structural decision.",
        );
    }

    prompt.push_str("\n\nChoose which side to keep:");
    prompt
}

#[must_use]
pub fn conflict_location_description(ours_change: &Change, theirs_change: &Change) -> String {
    let ours_location = ours_change.location();
    let theirs_location = theirs_change.location();

    if ours_location == theirs_location {
        format!("'{}'", ours_location.to_str_lossy())
    } else {
        format!(
            "ours='{}', theirs='{}'",
            ours_location.to_str_lossy(),
            theirs_location.to_str_lossy()
        )
    }
}

fn describe_change(change: &Change) -> String {
    match change {
        Change::Addition {
            location,
            entry_mode,
            ..
        } => {
            format!(
                "add {:?} at '{}'",
                entry_mode.kind(),
                location.to_str_lossy()
            )
        }
        Change::Deletion {
            location,
            entry_mode,
            ..
        } => {
            format!(
                "delete {:?} at '{}'",
                entry_mode.kind(),
                location.to_str_lossy()
            )
        }
        Change::Modification {
            location,
            previous_entry_mode,
            entry_mode,
            ..
        } => {
            format!(
                "modify '{}' ({:?} -> {:?})",
                location.to_str_lossy(),
                previous_entry_mode.kind(),
                entry_mode.kind()
            )
        }
        Change::Rewrite {
            source_location,
            location,
            source_entry_mode,
            entry_mode,
            copy,
            ..
        } => {
            let action = if *copy { "copy" } else { "rename" };
            format!(
                "{action} {:?} '{}' -> '{}' ({:?} -> {:?})",
                source_entry_mode.kind(),
                source_location.to_str_lossy(),
                location.to_str_lossy(),
                source_entry_mode.kind(),
                entry_mode.kind()
            )
        }
    }
}

fn is_blob_level_conflict(
    conflict: &gix::merge::tree::Conflict,
    ours_change: &Change,
    theirs_change: &Change,
) -> bool {
    conflict.content_merge().is_some()
        && ours_change.location() == theirs_change.location()
        && ours_change.entry_mode().is_blob_or_symlink()
        && theirs_change.entry_mode().is_blob_or_symlink()
}

fn entry_kind_for_shared_location(
    resolver: &dyn ConflictResolver,
    ours_change: &Change,
    theirs_change: &Change,
) -> Result<gix::objs::tree::EntryKind> {
    let ours_kind = ours_change.entry_mode().kind();
    let theirs_kind = theirs_change.entry_mode().kind();

    if ours_kind == theirs_kind {
        return Ok(ours_kind);
    }

    resolver.choose_entry_kind(ours_kind, theirs_kind)
}

fn edit_blob_in_editor(
    repo: &gix::Repository,
    resolver: &dyn ConflictResolver,
    blob_oid: ObjectId,
    path: &str,
) -> Result<ObjectId> {
    let blob = repo
        .find_blob(blob_oid)
        .with_context(|| format!("Failed to read merged blob '{blob_oid}' for conflict"))?;

    let edited_content = resolver
        .edit_blob(&blob.data, path)
        .context("Failed to edit blob for conflict resolution")?;
    Ok(repo.write_blob(edited_content)?.into())
}

fn apply_change_to_tree(tree: &mut gix::object::tree::Editor<'_>, change: &Change) -> Result<()> {
    match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        }
        | Change::Modification {
            location,
            entry_mode,
            id,
            ..
        } => {
            tree.upsert(location.as_bstr(), entry_mode.kind(), *id)?;
        }
        Change::Deletion { location, .. } => {
            tree.remove(location.as_bstr())?;
        }
        Change::Rewrite {
            source_location,
            location,
            entry_mode,
            id,
            copy,
            ..
        } => {
            if !*copy {
                tree.remove(source_location.as_bstr())?;
            }
            tree.upsert(location.as_bstr(), entry_mode.kind(), *id)?;
        }
    }

    Ok(())
}

fn remove_change_effect_from_tree(
    tree: &mut gix::object::tree::Editor<'_>,
    change: &Change,
) -> Result<()> {
    match change {
        Change::Addition { location, .. }
        | Change::Modification { location, .. }
        | Change::Rewrite { location, .. } => {
            tree.remove(location.as_bstr())?;
        }
        Change::Deletion { .. } => {}
    }

    Ok(())
}
