use anyhow::{Context, Result, bail, ensure};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use log::{debug, warn};

use crate::store::PorchettaStore;
use super::resolver::ConflictResolver;

/// Iterate over unresolved conflicts in `outcome` and resolve them using `resolver`.
///
/// # Errors
///
/// Returns an error if resolution fails or the user aborts.
pub fn resolve_conflicts(
    store: &PorchettaStore,
    resolver: &dyn ConflictResolver,
    outcome: &mut gix::merge::tree::Outcome<'_>,
) -> Result<()> {
    use gix::merge::tree::TreatAsUnresolved;
    for conflict in &outcome.conflicts {
        if !conflict.is_unresolved(TreatAsUnresolved::default()) {
            continue;
        }
        resolve_conflict(store, resolver, conflict, &mut outcome.tree)?;
    }
    Ok(())
}

fn resolve_conflict(
    store: &PorchettaStore,
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
        resolve_blob_level_conflict(store, resolver, conflict, merged_tree)?;
    } else {
        resolve_tree_level_conflict(resolver, conflict, merged_tree)?;
    }

    Ok(())
}

fn resolve_blob_level_conflict(
    store: &PorchettaStore,
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
    let edited_blob_id = edit_blob_in_editor(store, resolver, content_merge.merged_blob_id)?;

    let entry_kind = if ours_change.entry_mode().kind() == theirs_change.entry_mode().kind() {
        ours_change.entry_mode().kind()
    } else {
        entry_kind_for_shared_location(resolver, ours_change, theirs_change)?
    };

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
    ours_change: &gix::diff::tree_with_rewrites::Change,
    theirs_change: &gix::diff::tree_with_rewrites::Change,
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
        prompt.push_str("\n\nA merged blob exists, but this conflict still needs a structural decision.");
    }

    prompt.push_str("\n\nChoose which side to keep:");
    prompt
}

#[must_use]
pub fn conflict_location_description(
    ours_change: &gix::diff::tree_with_rewrites::Change,
    theirs_change: &gix::diff::tree_with_rewrites::Change,
) -> String {
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

fn describe_change(change: &gix::diff::tree_with_rewrites::Change) -> String {
    match change {
        gix::diff::tree_with_rewrites::Change::Addition { location, entry_mode, .. } => {
            format!("add {:?} at '{}'", entry_mode.kind(), location.to_str_lossy())
        }
        gix::diff::tree_with_rewrites::Change::Deletion { location, entry_mode, .. } => {
            format!("delete {:?} at '{}'", entry_mode.kind(), location.to_str_lossy())
        }
        gix::diff::tree_with_rewrites::Change::Modification {
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
        gix::diff::tree_with_rewrites::Change::Rewrite {
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
    ours_change: &gix::diff::tree_with_rewrites::Change,
    theirs_change: &gix::diff::tree_with_rewrites::Change,
) -> bool {
    conflict.content_merge().is_some()
        && ours_change.location() == theirs_change.location()
        && ours_change.entry_mode().is_blob_or_symlink()
        && theirs_change.entry_mode().is_blob_or_symlink()
}

fn entry_kind_for_shared_location(
    resolver: &dyn ConflictResolver,
    ours_change: &gix::diff::tree_with_rewrites::Change,
    theirs_change: &gix::diff::tree_with_rewrites::Change,
) -> Result<gix::objs::tree::EntryKind> {
    let ours_kind = ours_change.entry_mode().kind();
    let theirs_kind = theirs_change.entry_mode().kind();

    if ours_kind == theirs_kind {
        return Ok(ours_kind);
    }

    let prompt =
        "Local and remote entries have different kinds. Which should be used?";
    resolver.choose_entry_kind(prompt, ours_kind, theirs_kind)
}

fn edit_blob_in_editor(
    store: &PorchettaStore,
    resolver: &dyn ConflictResolver,
    blob_oid: ObjectId,
) -> Result<ObjectId> {
    let blob = store
        .find_blob(blob_oid)
        .with_context(|| format!("Failed to read merged blob '{blob_oid}' for conflict"))?;

    let edited_content = resolver
        .edit_blob(&blob.data)
        .context("Failed to edit blob for conflict resolution")?;
    Ok(store.write_blob(edited_content)?.into())
}

fn apply_change_to_tree(
    tree: &mut gix::object::tree::Editor<'_>,
    change: &gix::diff::tree_with_rewrites::Change,
) -> Result<()> {
    match change {
        gix::diff::tree_with_rewrites::Change::Addition {
            location,
            entry_mode,
            id,
            ..
        }
        | gix::diff::tree_with_rewrites::Change::Modification {
            location,
            entry_mode,
            id,
            ..
        } => {
            tree.upsert(location.as_bstr(), entry_mode.kind(), *id)?;
        }
        gix::diff::tree_with_rewrites::Change::Deletion { location, .. } => {
            tree.remove(location.as_bstr())?;
        }
        gix::diff::tree_with_rewrites::Change::Rewrite {
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
    change: &gix::diff::tree_with_rewrites::Change,
) -> Result<()> {
    match change {
        gix::diff::tree_with_rewrites::Change::Addition { location, .. }
        | gix::diff::tree_with_rewrites::Change::Modification { location, .. }
        | gix::diff::tree_with_rewrites::Change::Rewrite { location, .. } => {
            tree.remove(location.as_bstr())?;
        }
        gix::diff::tree_with_rewrites::Change::Deletion { .. } => {}
    }

    Ok(())
}
