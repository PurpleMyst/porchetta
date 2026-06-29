use mlua::{Lua, LuaSerdeExt, Table, Value};

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

/// Decode a JSON string into Lua tables and values.
///
/// JSON null is represented by `porchetta.json.null`.
///
/// # Errors
///
/// Returns a Lua runtime error if the input is not valid JSON.
#[allow(
    clippy::needless_pass_by_value,
    reason = "mlua callback arguments are received by value"
)]
pub fn json_decode(lua: &Lua, content: mlua::String) -> mlua::Result<Value> {
    let content = content.to_str()?;
    let value: serde_json::Value = serde_json::from_str(content.as_ref())
        .map_err(|e| mlua::Error::RuntimeError(format!("porchetta.json.decode: {e}")))?;
    lua.to_value(&value)
}

/// Encode a Lua value as compact JSON.
///
/// Use `porchetta.json.null` to encode JSON null.
///
/// # Errors
///
/// Returns a Lua runtime error if the Lua value cannot be represented as JSON.
pub fn json_encode(lua: &Lua, value: Value) -> mlua::Result<String> {
    let value: serde_json::Value = lua
        .from_value(value)
        .map_err(|e| mlua::Error::RuntimeError(format!("porchetta.json.encode: {e}")))?;
    serde_json::to_string(&value)
        .map_err(|e| mlua::Error::RuntimeError(format!("porchetta.json.encode: {e}")))
}

/// Encode a Lua value as pretty-printed JSON.
///
/// Use `porchetta.json.null` to encode JSON null.
///
/// # Errors
///
/// Returns a Lua runtime error if the Lua value cannot be represented as JSON.
pub fn json_encode_pretty(lua: &Lua, value: Value) -> mlua::Result<String> {
    let value: serde_json::Value = lua
        .from_value(value)
        .map_err(|e| mlua::Error::RuntimeError(format!("porchetta.json.encode_pretty: {e}")))?;
    serde_json::to_string_pretty(&value)
        .map_err(|e| mlua::Error::RuntimeError(format!("porchetta.json.encode_pretty: {e}")))
}

/// Create the `porchetta` Lua runtime table.
///
/// # Errors
///
/// Returns an error if any Lua function or table cannot be created.
pub fn create_porchetta_table(lua: &Lua) -> mlua::Result<Table> {
    let porchetta_tbl = lua.create_table()?;
    porchetta_tbl.set("system", lua.create_function(system)?)?;
    porchetta_tbl.set("hostname", lua.create_function(hostname)?)?;

    let json_tbl = lua.create_table()?;
    json_tbl.set("decode", lua.create_function(json_decode)?)?;
    json_tbl.set("encode", lua.create_function(json_encode)?)?;
    json_tbl.set("encode_pretty", lua.create_function(json_encode_pretty)?)?;
    json_tbl.set("null", lua.null())?;
    porchetta_tbl.set("json", json_tbl)?;

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

    #[test]
    fn test_json_decode_object() {
        let lua = lua_with_porchetta();
        let result: String = lua
            .load(r#"local v = porchetta.json.decode('{"name":"porchetta"}'); return v.name"#)
            .eval()
            .unwrap();
        assert_eq!(result, "porchetta");
    }

    #[test]
    fn test_json_decode_array() {
        let lua = lua_with_porchetta();
        let result: i64 = lua
            .load(r#"local v = porchetta.json.decode('[1,2,3]'); return v[2]"#)
            .eval()
            .unwrap();
        assert_eq!(result, 2);
    }

    #[test]
    fn test_json_encode_modified_object() {
        let lua = lua_with_porchetta();
        let result: String = lua
            .load(
                r#"
                local v = porchetta.json.decode('{"keep":true,"remove":"local"}')
                v.remove = nil
                v.added = "repo"
                return porchetta.json.encode(v)
                "#,
            )
            .eval()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(value, serde_json::json!({"keep": true, "added": "repo"}));
    }

    #[test]
    fn test_json_encode_pretty() {
        let lua = lua_with_porchetta();
        let result: String = lua
            .load(r#"return porchetta.json.encode_pretty({name = 'porchetta'})"#)
            .eval()
            .unwrap();
        assert!(result.contains('\n'));
        assert!(result.contains(r#""name": "porchetta""#));
    }

    #[test]
    fn test_json_null_distinct_from_missing_key() {
        let lua = lua_with_porchetta();
        let result: String = lua
            .load(
                r#"
                local v = porchetta.json.decode('{"present":null}')
                assert(v.present == porchetta.json.null)
                assert(v.missing == nil)
                v.missing = porchetta.json.null
                return porchetta.json.encode(v)
                "#,
            )
            .eval()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(value, serde_json::json!({"present": null, "missing": null}));
    }

    #[test]
    fn test_json_invalid_input_errors() {
        let lua = lua_with_porchetta();
        let result: mlua::Result<Value> = lua.load(r#"porchetta.json.decode('{')"#).eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("porchetta.json.decode"));
    }

    #[test]
    fn test_json_unsupported_lua_value_errors() {
        let lua = lua_with_porchetta();
        let result: mlua::Result<String> = lua
            .load(r#"porchetta.json.encode({callback = function() end})"#)
            .eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("porchetta.json.encode"));
    }
}
