use mlua::{Lua, Table};

/// Run a shell command and return its stdout.
///
/// Called from Lua as:
///
/// ```lua
/// local out = porchetta.system({"echo", "hi"})
/// local filtered = porchetta.system({"cat"}, "input text\n")
/// ```
///
/// # Errors
///
/// Returns a Lua runtime error if:
/// - the args table is empty,
/// - the command cannot be spawned,
/// - writing to stdin fails, or
/// - the command exits with a non-zero status.
pub fn system(lua: &Lua, (args, stdin): (Table, Option<String>)) -> mlua::Result<mlua::String> {
    let args: Vec<String> = args
        .sequence_values::<String>()
        .collect::<mlua::Result<_>>()?;
    if args.is_empty() {
        return Err(mlua::Error::RuntimeError(
            "porchetta.system: args table must not be empty".into(),
        ));
    }

    let mut cmd = std::process::Command::new(&args[0]);
    cmd.args(&args[1..]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }

    let mut child = cmd.spawn().map_err(|e| {
        mlua::Error::RuntimeError(format!(
            "porchetta.system: failed to spawn '{}': {e}",
            args[0]
        ))
    })?;

    if let Some(input) = stdin {
        use std::io::Write;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(input.as_bytes()).map_err(|e| {
                mlua::Error::RuntimeError(format!("porchetta.system: failed to write stdin: {e}"))
            })?;
        }
    }

    let output = child.wait_with_output().map_err(|e| {
        mlua::Error::RuntimeError(format!("porchetta.system: failed to read output: {e}"))
    })?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(mlua::Error::RuntimeError(format!(
            "porchetta.system: '{}' exited with code {code}: {stderr}",
            args.join(" ")
        )));
    }

    lua.create_string(&output.stdout)
}

/// Return the current hostname.
///
/// Called from Lua as:
///
/// ```lua
/// local name = porchetta.hostname()
/// ```
///
/// # Errors
///
/// Returns a Lua runtime error if the hostname cannot be determined.
pub fn hostname(lua: &Lua, _: ()) -> mlua::Result<mlua::String> {
    let hostname = ::hostname::get().map_err(|e| {
        mlua::Error::RuntimeError(format!("porchetta.hostname: failed to get hostname: {e}"))
    })?;
    lua.create_string(hostname.to_string_lossy().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_with_porchetta() -> Lua {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("system", lua.create_function(system).unwrap())
            .unwrap();
        tbl.set("hostname", lua.create_function(hostname).unwrap())
            .unwrap();
        lua.globals().set("porchetta", tbl).unwrap();
        lua
    }

    #[test]
    fn test_system_echo() {
        let lua = lua_with_porchetta();
        let result: String = lua
            .load(r#"porchetta.system({"echo", "hi"})"#)
            .eval()
            .unwrap();
        assert_eq!(result.trim(), "hi");
    }

    #[test]
    fn test_system_stdin() {
        let lua = lua_with_porchetta();
        let result: String = lua
            .load(r#"porchetta.system({"cat"}, "hello")"#)
            .eval()
            .unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_system_missing_command() {
        let lua = lua_with_porchetta();
        let result: mlua::Result<String> = lua
            .load(r#"porchetta.system({"porchetta-fake-binary-12345"})"#)
            .eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to spawn"));
    }

    #[test]
    fn test_system_non_zero_exit() {
        let lua = lua_with_porchetta();
        let result: mlua::Result<String> = lua.load(r#"porchetta.system({"false"})"#).eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exited with code"));
    }

    #[test]
    fn test_system_empty_args() {
        let lua = lua_with_porchetta();
        let result: mlua::Result<String> = lua.load(r#"porchetta.system({})"#).eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("args table must not be empty"));
    }

    #[test]
    fn test_hostname() {
        let lua = lua_with_porchetta();
        let result: String = lua.load(r#"porchetta.hostname()"#).eval().unwrap();
        assert!(!result.is_empty());
    }
}
