use std::process::Command;

fn tool_path(env_name: &str, fallback: &str) -> Option<String> {
    std::env::var(env_name)
        .ok()
        .or_else(|| which_in_path(fallback))
}

fn which_in_path(program: &str) -> Option<String> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(program))
            .find(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
    })
}

#[test]
fn manifest_edit_workspace_supports_luals_in_neovim() {
    if !cfg!(unix) {
        eprintln!("skipping: test editor script requires Unix");
        return;
    }

    let Some(nvim) = tool_path("NVIM", "nvim") else {
        eprintln!("skipping: set NVIM or put nvim in PATH");
        return;
    };
    let Some(lua_language_server) = tool_path("LUA_LANGUAGE_SERVER", "lua-language-server") else {
        eprintln!("skipping: set LUA_LANGUAGE_SERVER or put lua-language-server in PATH");
        return;
    };

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir(&home).unwrap();

    let bin = option_env!("CARGO_BIN_EXE_porchetta")
        .map(str::to_owned)
        .or_else(|| std::env::var("CARGO_BIN_EXE_porchetta").ok())
        .unwrap_or_else(|| "target/debug/porchetta".to_string());

    let status = Command::new(&bin)
        .arg("init")
        .env("HOME", &home)
        .status()
        .unwrap();
    assert!(status.success(), "porchetta init failed");

    let editor = temp.path().join("editor.sh");
    let nvim_lua = temp.path().join("check_luals.lua");

    std::fs::write(
        &nvim_lua,
        format!(
            r#"
local manifest = vim.fn.fnamemodify(vim.env.MANIFEST, ':p')
local workspace = vim.fn.fnamemodify(manifest, ':h')
local lua_ls = vim.env.LUA_LANGUAGE_SERVER

local function assert_file(path)
  if vim.fn.filereadable(path) ~= 1 then
    error('missing file: ' .. path)
  end
end

assert_file(workspace .. '/manifest.lua')
assert_file(workspace .. '/.luarc.json')
assert_file(workspace .. '/.lua-defs/porchetta.lua')

vim.cmd('cd ' .. vim.fn.fnameescape(workspace))
vim.cmd('edit manifest.lua')

local root = vim.fs.root(0, {{ '.luarc.json', '.git' }}) or vim.uv.cwd()
if root ~= workspace then
  error('unexpected LuaLS root: ' .. tostring(root) .. ', expected: ' .. workspace)
end

local client_id = vim.lsp.start({{
  name = 'porchetta_luals_e2e',
  cmd = {{ lua_ls }},
  root_dir = root,
  settings = {{}},
}})
if not client_id then
  error('failed to start lua-language-server')
end

local attached = vim.wait(5000, function()
  return #vim.lsp.get_clients({{ bufnr = 0, name = 'porchetta_luals_e2e' }}) > 0
end)
if not attached then
  error('lua-language-server did not attach')
end

vim.wait(3000, function()
  return false
end)

for _, diagnostic in ipairs(vim.diagnostic.get(0)) do
  if diagnostic.message:match('porchetta') and diagnostic.message:lower():match('undefined') then
    error('unexpected porchetta diagnostic: ' .. diagnostic.message)
  end
end

local function request(method, params)
  local result = vim.lsp.buf_request_sync(0, method, params, 5000)
  if not result or vim.tbl_isempty(result) then
    error(method .. ' returned no result')
  end
  for _, response in pairs(result) do
    if response.result then
      return response.result
    end
  end
  error(method .. ' returned no successful response: ' .. vim.inspect(result))
end

local hover_params = vim.lsp.util.make_position_params(0, 'utf-8')
hover_params.position = {{ line = 0, character = 14 }}
local hover = request('textDocument/hover', hover_params)
local hover_text = vim.inspect(hover)
if not hover_text:match('Porchetta') then
  error('hover did not include Porchetta type: ' .. hover_text)
end

local completion_params = vim.lsp.util.make_position_params(0, 'utf-8')
completion_params.position = {{ line = 2, character = 28 }}
completion_params.context = {{ triggerKind = 1 }}
local completion = request('textDocument/completion', completion_params)
local labels = {{}}
for _, item in ipairs(completion.items or completion) do
  labels[item.label] = true
end
for _, label in ipairs({{ 'decode(content)', 'encode(value)', 'encode_pretty(value)', 'null' }}) do
  if not labels[label] then
    error('missing completion ' .. label .. ': ' .. vim.inspect(labels))
  end
end

vim.cmd('qa!')
"#,
        ),
    )
    .unwrap();

    std::fs::write(
        &editor,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
manifest="$1"
cat > "$manifest" <<'LUA'
local host = porchetta.hostname()
local raw = porchetta.system({{"printf", "{{}}"}})
local data = porchetta.json.decode(raw)
data.empty = porchetta.json.null
local out = porchetta.json.encode_pretty(data)

return {{
  topics = {{}},
  should_include = function(path)
    return host ~= "" and out ~= path
  end,
}}
LUA
MANIFEST="$manifest" LUA_LANGUAGE_SERVER="{}" "{}" --headless -u NONE -i NONE -S "{}"
"#,
            lua_language_server,
            nvim,
            nvim_lua.display()
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&editor).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&editor, permissions).unwrap();
    }

    let output = Command::new(&bin)
        .args(["manifest", "edit"])
        .env("HOME", &home)
        .env("EDITOR", &editor)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "porchetta manifest edit failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
