use std::process::Command;

#[test]
fn store_path_is_pipeable_even_in_quiet_mode() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_porchetta"));
    command.args(["--quiet", "store", "path"]);
    #[cfg(not(unix))]
    let home = porchetta::util::home_dir().unwrap();
    // On Unix this also proves discovery works before initialization. Windows
    // known-folder resolution does not respect HOME/USERPROFILE overrides.
    #[cfg(unix)]
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    let home = {
        command.env("HOME", temp.path());
        camino::Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap()
    };
    let output = command.output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stderr.as_slice(), b"");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n", home.join(".porchetta"))
    );
    #[cfg(unix)]
    assert!(!home.join(".porchetta").exists());
}
