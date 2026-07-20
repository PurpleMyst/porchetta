use mlua::{Lua, LuaSerdeExt, Table, Value};

/// Create the `porchetta.json` Lua runtime table.
///
/// # Errors
///
/// Returns an error if any Lua function or table cannot be created.
pub(super) fn create_table(lua: &Lua) -> mlua::Result<Table> {
    let json_tbl = lua.create_table()?;
    json_tbl.set("decode", lua.create_function(decode)?)?;
    json_tbl.set("encode", lua.create_function(encode)?)?;
    json_tbl.set("encode_pretty", lua.create_function(encode_pretty)?)?;
    json_tbl.set("null", lua.null())?;
    Ok(json_tbl)
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
fn decode(lua: &Lua, content: mlua::String) -> mlua::Result<Value> {
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
fn encode(lua: &Lua, value: Value) -> mlua::Result<String> {
    encode_with(lua, value, "porchetta.json.encode", serde_json::to_string)
}

/// Encode a Lua value as pretty-printed JSON.
///
/// Use `porchetta.json.null` to encode JSON null.
///
/// # Errors
///
/// Returns a Lua runtime error if the Lua value cannot be represented as JSON.
fn encode_pretty(lua: &Lua, value: Value) -> mlua::Result<String> {
    encode_with(
        lua,
        value,
        "porchetta.json.encode_pretty",
        serde_json::to_string_pretty,
    )
}

fn encode_with(
    lua: &Lua,
    value: Value,
    context: &str,
    formatter: fn(&serde_json::Value) -> serde_json::Result<String>,
) -> mlua::Result<String> {
    let value = lua_to_json(lua, value, context)?;
    formatter(&value).map_err(|e| mlua::Error::RuntimeError(format!("{context}: {e}")))
}

fn lua_to_json(lua: &Lua, value: Value, context: &str) -> mlua::Result<serde_json::Value> {
    lua.from_value(value)
        .map_err(|e| mlua::Error::RuntimeError(format!("{context}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_with_json() -> Lua {
        let lua = Lua::new();
        let porchetta = lua.create_table().unwrap();
        porchetta.set("json", create_table(&lua).unwrap()).unwrap();
        lua.globals().set("porchetta", porchetta).unwrap();
        lua
    }

    #[test]
    fn test_decode_object() {
        let lua = lua_with_json();
        let result: String = lua
            .load(r#"local v = porchetta.json.decode('{"name":"porchetta"}'); return v.name"#)
            .eval()
            .unwrap();
        assert_eq!(result, "porchetta");
    }

    #[test]
    fn test_decode_array() {
        let lua = lua_with_json();
        let result: i64 = lua
            .load(r"local v = porchetta.json.decode('[1,2,3]'); return v[2]")
            .eval()
            .unwrap();
        assert_eq!(result, 2);
    }

    #[test]
    fn test_encode_modified_object() {
        let lua = lua_with_json();
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
    fn test_encode_pretty() {
        let lua = lua_with_json();
        let result: String = lua
            .load(r"return porchetta.json.encode_pretty({name = 'porchetta'})")
            .eval()
            .unwrap();
        assert!(result.contains('\n'));
        assert!(result.contains(r#""name": "porchetta""#));
    }

    #[test]
    fn test_null_distinct_from_missing_key() {
        let lua = lua_with_json();
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
    fn test_decoded_empty_array_round_trips_as_array() {
        let lua = lua_with_json();
        let result: String = lua
            .load(r"return porchetta.json.encode(porchetta.json.decode('[]'))")
            .eval()
            .unwrap();
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_decoded_empty_object_round_trips_as_object() {
        let lua = lua_with_json();
        let result: String = lua
            .load(r"return porchetta.json.encode(porchetta.json.decode('{}'))")
            .eval()
            .unwrap();
        assert_eq!(result, "{}");
    }

    #[test]
    fn test_nested_empty_array_round_trips_as_array() {
        let lua = lua_with_json();
        let result: String = lua
            .load(
                r#"
                local v = porchetta.json.decode('{"items":[]}')
                return porchetta.json.encode(v)
                "#,
            )
            .eval()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(value, serde_json::json!({"items": []}));
    }

    #[test]
    fn test_decoded_array_round_trips_after_mutation() {
        let lua = lua_with_json();
        let result: String = lua
            .load(
                r"
                local v = porchetta.json.decode('[1,2]')
                v[3] = 3
                return porchetta.json.encode(v)
                ",
            )
            .eval()
            .unwrap();
        assert_eq!(result, "[1,2,3]");
    }

    #[test]
    fn test_lua_created_empty_table_encodes_as_object() {
        let lua = lua_with_json();
        let result: String = lua
            .load(r"return porchetta.json.encode({})")
            .eval()
            .unwrap();
        assert_eq!(result, "{}");
    }

    #[test]
    fn test_invalid_input_errors() {
        let lua = lua_with_json();
        let result: mlua::Result<Value> = lua.load(r"porchetta.json.decode('{')").eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("porchetta.json.decode"));
    }

    #[test]
    fn test_unsupported_lua_value_errors() {
        let lua = lua_with_json();
        let result: mlua::Result<String> = lua
            .load(r"porchetta.json.encode({callback = function() end})")
            .eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("porchetta.json.encode"));
    }
}
