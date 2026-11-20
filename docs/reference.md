# Porchetta reference

For a first setup and everyday commands, start with the [README](../README.md).

## Manifest

The manifest is Lua that returns a table of topics and an optional global
`should_include` function. Porchetta evaluates it on each machine, so it can choose
paths and enable topics using the operating system, environment or hostname.

### Topic fields

| Field | Meaning |
|-------|---------|
| `enabled` | Whether to capture and apply this topic on this machine; defaults to `true`. |
| `root` | Base directory under home. Relative roots start at home; absolute roots within home are normalized to relative paths. |
| `paths` | Files or directories relative to `root`, or home when `root` is omitted. Use `"."` to capture the whole directory. |
| `symlinks` | Map of link paths to targets; link paths are relative to the topic's base directory. |
| `to_repo(path, content)` | Return the content to store, for example after replacing machine-specific paths. |
| `to_system(path, content, current)` | Return the content to write on add or update. `current` is the existing file's raw bytes, or `nil` if absent. |
| `should_include(path)` | Return a boolean to include or exclude a file or directory while scanning. |

Topic and global `should_include` functions must both return `true` to include a
path. Excluding a directory also skips its descendants. Paths passed to hooks are
relative to the topic's base directory and use `/` separators.

For example, exclude generated directories from a topic that captures its whole root:

```lua
return {
    topics = {
        nvim = { root = ".config/nvim", paths = { "." } },
    },
    should_include = function(path)
        return not ("/" .. path .. "/"):find("/node_modules/", 1, true)
            and not ("/" .. path .. "/"):find("/.venv/", 1, true)
    end,
}
```

Lua and hooks run with the user's permissions. Use trusted manifests; validation
and inspection also evaluate Lua. They do not execute content or filter hooks.
Dry runs execute those hooks to calculate changes.

### Platform-dependent paths

`root` can be computed from environment variables:

```lua
local windows = package.config:sub(1, 1) == "\\"
local nvim_root = windows and (os.getenv("LOCALAPPDATA") .. "/nvim") or ".config/nvim"

return {
    topics = {
        nvim = { root = nvim_root, paths = { "init.lua", "lua", "snippets" } },
        shell = { enabled = not windows, paths = { ".bashrc" } },
    },
}
```

Use `porchetta manifest inspect --topic nvim` to see the evaluated configuration
on the current machine. Inspection includes disabled topics and reports which
hooks are present. Use `manifest view` to read the original Lua and hook bodies.

### Content transforms

A pair of hooks can replace home paths with a placeholder in the store and restore
them on each machine:

```lua
local windows = package.config:sub(1, 1) == "\\"
local home = os.getenv(windows and "USERPROFILE" or "HOME"):gsub("\\", "/")

local function replace_literal(content, from, to)
    local pattern = from:gsub("(%W)", "%%%1")
    return (content:gsub(pattern, function() return to end))
end

return {
    topics = {
        git = {
            paths = { ".gitconfig" },
            to_repo = function(_, content)
                content = replace_literal(content, home, "__HOME__")
                return replace_literal(content, home:gsub("/", "\\"), "__HOME__")
            end,
            to_system = function(_, content)
                return replace_literal(content, "__HOME__", home)
            end,
        },
    },
}
```

For valid UTF-8 content with no NUL byte, Porchetta normalizes CR-LF line endings
to LF before and after `to_repo`, and before `to_system`.

### Keep settings local to each machine

Keep using Pi as usual. To sync `~/.pi/agent/settings.json` without sharing your
thinking level or which changelog you last saw, have `to_repo` leave those keys
out of the store and `to_system` copy them from the file on this machine:

```lua
local local_keys = { "defaultThinkingLevel", "lastChangelogVersion" }

return {
    topics = {
        pi_agent = {
            root = ".pi/agent",
            paths = { "settings.json" },
            to_repo = function(_, content)
                local settings = porchetta.json.decode(content)
                for _, key in ipairs(local_keys) do settings[key] = nil end
                return porchetta.json.encode_pretty(settings)
            end,
            to_system = function(_, content, current)
                local settings = porchetta.json.decode(content)
                local existing = current and porchetta.json.decode(current) or {}
                for _, key in ipairs(local_keys) do settings[key] = existing[key] end
                return porchetta.json.encode_pretty(settings)
            end,
        },
    },
}
```

If a key isn't set on this machine, the hook leaves it unset. That includes a
fresh install with no settings file.

`current` contains the file as it is on disk when Porchetta applies the change,
with line endings untouched. Hooks that don't need it can still take two
arguments. If sync deletes the file, it deletes the whole file without calling
the hook. This example doesn't protect against Pi saving at the same time or
remove values from old commits.

### Symlinks and topic references

Symlink targets can be literal relative path strings or references to another topic:

```lua
return {
    topics = {
        shell = { root = ".config/shell", paths = { "common.sh" } },
        bash = {
            paths = { ".bashrc" },
            symlinks = {
                [".shell-common.sh"] = porchetta.topic("shell") / "common.sh",
                [".shell-config"] = porchetta.topics.shell,
            },
        },
    },
}
```

`porchetta.topic(name)` and `porchetta.topics.<name>` create symbolic topic
references. `/` appends a path. Porchetta computes the relative link target across
the topics' roots.

### Lua runtime helpers

- `porchetta.hostname()` returns the current hostname.
- `porchetta.system(args[, stdin])` runs an external command and returns stdout.
  Use it to call formatters or generators from hooks.
- `porchetta.json.decode(content)` parses JSON into Lua tables and values.
- `porchetta.json.encode(value)` produces compact JSON.
- `porchetta.json.encode_pretty(value)` produces formatted JSON.
- `porchetta.json.null` represents JSON `null`; an absent key is Lua `nil`.

Decoded arrays and objects retain their shape when re-encoded, including empty
arrays and objects. Lua-created empty tables encode as objects. To create an empty
array, decode `"[]"` and then modify it.

For example, a JSON transform can remove a machine-specific key:

```lua
to_repo = function(_, content)
    local settings = porchetta.json.decode(content)
    settings.machineId = nil
    return porchetta.json.encode_pretty(settings)
end
```

### LuaLS support

`porchetta manifest edit` opens a temporary LuaLS workspace with `.luarc.json` and
Porchetta's runtime definitions. Editors with LuaLS configured can use these for
completions and hover documentation.

For manifests edited outside that command, add the
[official stub](../src/luals/porchetta.lua) to your LuaLS workspace library:

```json
{
  "runtime": { "version": "Lua 5.4" },
  "workspace": {
    "library": ["/path/to/porchetta/src/luals"],
    "checkThirdParty": false
  }
}
```

## Synchronization

### Merge model

After fetching remotes, Porchetta reconciles their topic and manifest histories
with the local store. It then merges three filesystem trees for each topic:

- **Base:** the last state applied on this hostname (`system/<hostname>/<topic>`).
- **Ours:** the topic's current local files.
- **Theirs:** the reconciled stored state (`topic/<topic>`).

The merged tree is captured to the topic branch when it differs from theirs,
applied to the filesystem when it differs from ours, and recorded as the new base.

With `sync --offline`, Porchetta uses the local store and skips fetching,
remote-history reconciliation and publication.

### Topic selection

`sync --topic NAME` (or `-t`) selects topics for capture, apply and reconciliation
with fetched histories. Repeat the flag to select more than one. Unknown names
fail before any topic is synchronized, with the available names in the error.

A filtered sync publishes the manifest and the selected topic branches. Unselected
topic branches are left untouched on remotes. Disabled topics can still have
their stored history reconciled and published, while their local files are skipped.

`manifest inspect --topic NAME` filters the configuration report rather than the
manifest's Lua evaluation. It includes disabled topics and preserves the report's
revision and global hook information.

### Dry runs

`sync --dry-run` previews capture and apply changes. It leaves canonical `manifest`
and `topic/*` refs, local `system/*` refs, managed filesystem contents, refs on remotes
and repository configuration unchanged. Online previews still fetch, which can
refresh remote-tracking refs. Lua and hooks execute during preview.

Modeling changes may write unreachable Git blobs, trees and commits. They do not
change the synchronized state and can be reclaimed by Git garbage collection.

A conflicting topic is reported and preview continues with the remaining topics.
A conflicting manifest history requires an interactive sync, because the manifest
defines the topic set.

A clean report means there are no capture/apply changes for the selected topics
in the examined state. Unchanged, excluded and unconfigured files can all be absent
from the report; use the manifest to check configured paths and filters.

### Remotes and publication failures

Remotes are symmetric: `origin` has no special role. Each normal online sync
fetches every configured remote before synchronizing local files, then publishes
to all of them. A clone retains its source as `origin`.

```sh
porchetta remote add backup ssh://backup.example/you/dotfiles.git
porchetta remote list
porchetta remote remove backup
```

If a fetch fails, sync stops before capturing, merging or applying local files.
After local sync succeeds, Porchetta attempts every publication target, even if
one fails. Any failed push produces a publication error; local commits and file
changes remain complete. Fix the remote and rerun sync to publish.

### Conflict prompts

- **Content conflicts:** the merged file opens in your editor for resolution.
- **Tree conflicts:** add/delete/rename or file/directory disagreements prompt you
  to choose which side to keep.
- **Symlink conflicts:** an existing path at the link location prompts to replace
  it with the symlink, keep it, or abort.

## Storage

`porchetta store path` prints the store location, normally `~/.porchetta`.
The store is a bare Git repository containing these branches:

- `manifest`: the `manifest.lua` source.
- `topic/<name>`: committed content for a topic.
- `system/<hostname>/<name>`: the last state applied on a machine. These bookkeeping
  branches stay local.

Porchetta permits one active process per store. Another process stops before
accessing it. Wait for Porchetta to finish before making direct Git or filesystem
changes to the store.

## Migrate from chezmoi

For a new Porchetta store:

```sh
porchetta init
porchetta migrate chezmoi --source-dir ~/.local/share/chezmoi
porchetta manifest edit
porchetta sync --dry-run
porchetta sync
```

The importer replaces the manifest after confirmation. Review the imported paths
in the editor before syncing.
Omit `--source-dir` to use `~/.local/share/chezmoi`; `--yes` skips the overwrite prompt.

## Build from a checkout

```sh
git clone https://github.com/PurpleMyst/porchetta.git
cd porchetta
cargo install --locked --path .
```

`--locked` uses the dependency versions recorded in `Cargo.lock`.

## Command help

Use `porchetta --help` to list commands. Each command's `--help` describes its
options and examples. `porchetta manifest --help` documents the revision-checked
workflow for non-interactive edits; its subcommand help documents JSON formats
and exit codes.
