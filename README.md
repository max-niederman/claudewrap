# claudewrap

Sandbox [Claude Code](https://docs.anthropic.com/en/docs/claude-code) (or any command) using [bubblewrap](https://github.com/containers/bubblewrap). The filesystem is read-only by default, with explicit write access granted to specific paths. SSH is isolated to a dedicated sandbox key.

## How it works

claudewrap builds a `bwrap` command that:

1. Bind-mounts `/` read-only as the base layer
2. Grants write access to the current git repo, `~/.claude`, and any paths from config
3. Spawns a dedicated `ssh-agent` loaded with `~/.ssh/id_claudewrap_ed25519` only — the sandbox never sees your real keys, and can only authenticate to hosts where you've added the sandbox public key
4. Overrides git's signing config to use the sandbox key
5. Optionally forwards Wayland, PipeWire, and D-Bus sockets

## Requirements

- Linux with [bubblewrap](https://github.com/containers/bubblewrap) installed
- `ssh-agent` and `ssh-keygen` (from OpenSSH)

## Install

```
cargo install --path .
```

## Usage

```
claudewrap [OPTIONS] [-- ARGS...]
```

On first run, claudewrap prompts to create `~/.ssh/id_claudewrap_ed25519`. Add the public key to GitHub so the sandbox can push.

### Examples

```sh
# Run Claude Code in the sandbox (default)
claudewrap

# Run a different command
claudewrap -e bash

# Grant extra write access
claudewrap -w /tmp/scratch -w ~/other-project

# Preview the bwrap command
claudewrap --dry-run
```

### Options

| Flag | Description |
|---|---|
| `-e, --exec CMD` | Command to run (default: `claude`) |
| `-w, --write PATH` | Grant write access (repeatable) |
| `-s, --scope ID` | Activate a named scope (repeatable) |
| `--wayland` / `--no-wayland` | Force Wayland socket on/off |
| `--pipewire` | Enable PipeWire socket |
| `--dbus MODE` | Enable D-Bus (`session` or `system`) |
| `--dry-run` | Print bwrap command without executing |
| `--verbose` | Show resolved config |

## Configuration

Place `.claude/wrap.toml` in any parent directory. claudewrap walks up from the current directory and merges all configs found (permissions are OR-merged).

```toml
[scope]
id = "myproject"     # optional name for -s selection
default = true       # auto-activate this scope

[filesystem]
write = ["./build", "/tmp/myproject"]

[sockets]
wayland = false
pipewire = false
dbus = false         # or "session" / "system"
```

## License

MIT
