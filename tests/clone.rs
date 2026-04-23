use camino::Utf8PathBuf;

#[test]
fn test_clone_from_local_repo() {
    let temp = tempfile::tempdir().unwrap();
    let source = Utf8PathBuf::try_from(temp.path().join("source")).unwrap();
    let dest = Utf8PathBuf::try_from(temp.path().join("dest")).unwrap();

    // Create a proper Porchetta store (bare repo with manifest branch).
    let source_store = porchetta::store::PorchettaStore::init_at(&source).unwrap();
    let manifest = b"return { topics = { dots = { paths = {'.bashrc'} } } }\n";
    source_store.write_manifest(manifest).unwrap();

    // Clone the store into the destination path.
    let store = porchetta::store::PorchettaStore::clone_from(source.as_str(), &dest)
        .expect("clone_from should succeed");

    // Verify the manifest is readable.
    let cloned_manifest = store.read_manifest().expect("should read cloned manifest");
    assert!(String::from_utf8_lossy(&cloned_manifest).contains("dots"));
}

#[test]
fn test_clone_from_fails_when_dest_exists() {
    let temp = tempfile::tempdir().unwrap();
    let dest = Utf8PathBuf::try_from(temp.path().join("dest")).unwrap();

    // Create the destination directory so clone should fail.
    std::fs::create_dir(&dest).unwrap();

    let result = porchetta::store::PorchettaStore::clone_from("/dev/null/nonexistent", &dest);
    assert!(result.is_err());
}
