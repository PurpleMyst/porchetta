mod json;

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
fn system(lua: &Lua, (args, stdin): (Table, Option<String>)) -> mlua::Result<mlua::LuaString> {
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
fn hostname(lua: &Lua, _: ()) -> mlua::Result<mlua::LuaString> {
    let hostname = ::hostname::get().map_err(|e| {
        mlua::Error::RuntimeError(format!("porchetta.hostname: failed to get hostname: {e}"))
    })?;
    lua.create_string(hostname.to_string_lossy().as_bytes())
}

fn create_topic_ref(lua: &Lua, topic_name: String, path: Option<String>) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;
    tbl.set("__topic", topic_name)?;
    if let Some(p) = path {
        tbl.set("__path", p)?;
    }

    let mt = lua.create_table()?;
    mt.set(
        "__div",
        lua.create_function(|lua, (target, subpath): (Table, String)| {
            let topic: String = target.get("__topic")?;
            let current_path: Option<String> = target.get("__path")?;
            let next_path = match current_path {
                Some(existing) => format!("{existing}/{subpath}"),
                None => subpath,
            };
            create_topic_ref(lua, topic, Some(next_path))
        })?,
    )?;
    mt.set(
        "__tostring",
        lua.create_function(|_lua, target: Table| {
            let topic: String = target.get("__topic")?;
            let current_path: Option<String> = target.get("__path")?;
            match current_path {
                Some(p) => Ok(format!("porchetta.topic('{topic}')/{p}")),
                None => Ok(format!("porchetta.topic('{topic}')")),
            }
        })?,
    )?;

    tbl.set_metatable(Some(mt))?;
    Ok(tbl)
}

/// Create the `porchetta` Lua runtime table.
///
/// # Errors
///
/// Returns an error if any Lua function or table cannot be created.
pub(crate) fn create_porchetta_table(lua: &Lua) -> mlua::Result<Table> {
    let porchetta_tbl = lua.create_table()?;
    porchetta_tbl.set("system", lua.create_function(system)?)?;
    porchetta_tbl.set("hostname", lua.create_function(hostname)?)?;
    porchetta_tbl.set("json", json::create_table(lua)?)?;

    let topic_fn = lua.create_function(|lua, name: String| create_topic_ref(lua, name, None))?;
    porchetta_tbl.set("topic", topic_fn.clone())?;

    let topics_tbl = lua.create_table()?;
    let topics_mt = lua.create_table()?;
    topics_mt.set(
        "__index",
        lua.create_function(move |lua, (_, name): (Table, String)| {
            create_topic_ref(lua, name, None)
        })?,
    )?;
    topics_tbl.set_metatable(Some(topics_mt))?;
    porchetta_tbl.set("topics", topics_tbl)?;

    Ok(porchetta_tbl)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_with_porchetta() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("porchetta", create_porchetta_table(&lua).unwrap())
            .unwrap();
        lua
    }

    #[test]
    fn test_system_echo() {
        let lua = lua_with_porchetta();
        let result: String = lua
            .load(r#"porchetta.system({"git", "--version"})"#)
            .eval()
            .unwrap();
        assert!(result.starts_with("git version"));
    }

    #[test]
    fn test_system_stdin() {
        let lua = lua_with_porchetta();
        let result: String = lua
            .load(r#"porchetta.system({"git", "hash-object", "--stdin"}, "hello\n")"#)
            .eval()
            .unwrap();
        assert_eq!(result.trim(), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn test_topic_ref_div_operator() {
        let lua = lua_with_porchetta();
        let result: Table = lua
            .load(r#"porchetta.topic("pi_agent") / "AGENTS.md""#)
            .eval()
            .unwrap();
        assert_eq!(result.get::<String>("__topic").unwrap(), "pi_agent");
        assert_eq!(result.get::<String>("__path").unwrap(), "AGENTS.md");

        let tostring: String = lua
            .load(r#"tostring(porchetta.topic("pi_agent") / "AGENTS.md")"#)
            .eval()
            .unwrap();
        assert_eq!(tostring, "porchetta.topic('pi_agent')/AGENTS.md");

        // Chaining
        let chained: Table = lua
            .load(r#"porchetta.topics.pi_agent / "skills" / "sub""#)
            .eval()
            .unwrap();
        assert_eq!(chained.get::<String>("__topic").unwrap(), "pi_agent");
        assert_eq!(chained.get::<String>("__path").unwrap(), "skills/sub");

        // Bare topic
        let bare: Table = lua.load(r"porchetta.topics.pi_agent").eval().unwrap();
        assert_eq!(bare.get::<String>("__topic").unwrap(), "pi_agent");
        assert!(bare.get::<Option<String>>("__path").unwrap().is_none());
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
        let result: mlua::Result<String> = lua
            .load(r#"porchetta.system({"git", "not-a-git-command"})"#)
            .eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exited with code"));
    }

    #[test]
    fn test_system_empty_args() {
        let lua = lua_with_porchetta();
        let result: mlua::Result<String> = lua.load(r"porchetta.system({})").eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("args table must not be empty"));
    }

    #[test]
    fn test_hostname() {
        let lua = lua_with_porchetta();
        let result: String = lua.load(r"porchetta.hostname()").eval().unwrap();
        assert_ne!(result, "");
    }
}
