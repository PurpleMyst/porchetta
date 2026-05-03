# Porchetta

Porchetta is a dotfile manager that synchronizes configuration *topics* between your local machine and a Git store using three-way merge. It treats your dotfiles as first-class content that can change on either side, resolving conflicts interactively when they occur.

## The Idea

Most dotfile managers are one-directional: they copy files from a repository to your home directory. If you edit a file locally, you must remember to commit the change manually.

Porchetta is bidirectional. Each *topic* (e.g., `shell`, `git`, `nvim`) is tracked independently. When you sync:

1. **Capture** — reads the current state of your local files.
2. **Merge** — three-way merges local changes with the stored topic branch, using your machine's last-applied state as the base.
3. **Apply** — writes merged changes back to your filesystem and commits the result.

This means you can edit a config file on one machine, sync to push it, then sync on another machine to pull the change — with automatic conflict resolution when both sides diverge.

## Installation

```bash
cargo install --path .
```

Porchetta stores its bare Git repository at `~/.porchetta`.

## Quick Start

```bash
porchetta init
porchetta edit   # edit manifest.lua
porchetta sync
```

## Manifest

The manifest is a Lua file that declares topics. Each topic groups related paths under a name.

```lua
return {
    topics = {
        shell = {
            paths = {".bashrc", ".zshrc", ".inputrc"}
        },
        git = {
            paths = {".gitconfig", ".gitignore_global"}
        },
        nvim = {
            root = ".config/nvim",
            paths = {"init.lua", "lua/plugins.lua"}
        }
    }
}
```

- `paths` — files or directories to sync, relative to `root` (or home if `root` is omitted).
- `root` — base directory for the topic's paths.

### Hooks

Topics can transform content on the way into or out of the store. This lets you keep secrets out of the repository while still managing the file as a topic.

**NB**: Currently we normalize all valid UTF-8 so that CR-LF becomes just LF; this happens *after*
the `to_repo` hook and *before* the `to_system` hook. In the future this might be more configurable.

```lua
return {
    topics = {
        git = {
            paths = {".gitconfig"},
            to_repo = function(path, content)
                return content:gsub("token = .-\n", "token = <redacted>\n")
            end,
            to_system = function(path, content)
                local token = os.getenv("GITHUB_TOKEN") or ""
                return content:gsub("token = <redacted>", "token = " .. token)
            end,
            should_include = function(path)
                return path:sub(-4) ~= ".bak"
            end
        }
    },
    should_include = function(path)
        return path:sub(-4) ~= ".tmp"
    end
}
```

- `to_repo(path, content) -> string` — called before storing topic content in the repo.
- `to_system(path, content) -> string` — called before writing topic content to the filesystem.
- `should_include(path) -> boolean` — filters files or directories while scanning. It can be set either on a topic or at the top level of the manifest; both must return `true` for a path to be included.

Manifest Lua code also has access to `porchetta.system(args[, stdin])`, which runs an external command and returns its stdout as a string.

## Commands

| Command | Description |
|---------|-------------|
| `porchetta init` | Create a new store at `~/.porchetta`. |
| `porchetta edit` | Open `manifest.lua` in `$EDITOR`. |
| `porchetta show` | Print the current manifest. |
| `porchetta sync` | Synchronize all topics. |
| `porchetta sync --dry-run` | Preview what would change without applying. |
| `porchetta sync --offline` | Sync without fetching from or pushing to `origin`. |
| `porchetta clone <url>` | Clone a remote store. Run `sync` next. |
| `porchetta migrate chezmoi` | Import topics from a chezmoi source directory. Use `--source-dir <dir>` to override the default and `--yes` to skip the overwrite prompt. |

## Workflow Examples

### Set up a new machine

```bash
porchetta clone git@github.com:you/dotfiles.git
porchetta sync
```

### Edit and propagate

```bash
# Edit ~/.bashrc locally, then push it to the store
porchetta sync

# On another machine, pull the change
porchetta sync
```

### Preview or work offline

```bash
porchetta sync --dry-run
porchetta sync --offline
```

### Migrate from chezmoi

```bash
porchetta init
porchetta migrate chezmoi --source-dir ~/.local/share/chezmoi
porchetta sync
```

## How Sync Works

For each topic, Porchetta builds three trees:

- **Base** — the last state applied to this hostname (`system/<hostname>/<topic>`).
- **Ours** — the current state of the topic's paths on the local filesystem.
- **Theirs** — the stored state of the topic (`topic/<topic>`).

It performs a three-way merge of these trees. The result is:

- **Pushed** to the topic branch if it differs from `theirs`.
- **Pulled** to the filesystem if it differs from `ours`.
- **Recorded** as the new base for this hostname.

### Conflict Resolution

If a topic diverges on both sides, Porchetta prompts for resolution:

- **Blob conflicts** (same file modified) — opens the merged file in your `$EDITOR` for hand editing.
- **Tree conflicts** (add/delete/rename) — prompts to keep local, keep remote, or abort.

## Storage Layout

The store at `~/.porchetta` is a bare Git repository with these branches:

- `manifest` — the `manifest.lua` file.
- `topic/<name>` — the latest committed state of each topic.
- `system/<hostname>/<name>` — the last state applied to each machine.

Because topics live on independent branches, you can sync them individually and never deal with merge conflicts across unrelated configs.
