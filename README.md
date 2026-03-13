# claudewrap

Sandbox [Claude Code](https://docs.anthropic.com/en/docs/claude-code) (or any command) using [bubblewrap](https://github.com/containers/bubblewrap). The filesystem is read-only by default, with explicit write access granted to specific paths. SSH access is optionally passed through from your host agent, filtered by key fingerprint.

## How it works

claudewrap builds a `bwrap` command that:

1. Bind-mounts `/` read-only as the base layer
2. Grants write access to the current git repo, `~/.claude`, and any paths from config
3. Optionally passes through the host SSH agent socket, after verifying it contains all configured key fingerprints — the sandbox sees only the host agent, not your private key files
4. Overrides git's signing config to use the first matched key (via `key::` literal format)
5. Optionally forwards Wayland, PipeWire, and D-Bus sockets

## Requirements

- Linux with [bubblewrap](https://github.com/containers/bubblewrap) installed
- An SSH agent running with the desired key(s) loaded (`ssh-add`), if SSH is enabled

## Install

```
cargo install --path .
```

## Usage

```
claudewrap [OPTIONS] [-- ARGS...]
```

### Examples

```sh
# Run Claude Code in the sandbox (default)
claudewrap

# Add an SSH key fingerprint (implies agent = true)
claudewrap --ssh-key SHA256:abc123...

# Multiple keys
claudewrap --ssh-key SHA256:abc... --ssh-key SHA256:def...

# Run without SSH even if config enables it
claudewrap --no-ssh-agent

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
| `--ssh-key FINGERPRINT` | Allow SSH key from host agent (repeatable; implies agent passthrough) |
| `--no-ssh-agent` | Disable SSH agent passthrough |
| `--wayland` / `--no-wayland` | Force Wayland socket on/off |
| `--pipewire` | Enable PipeWire socket |
| `--dbus MODE` | Enable D-Bus (`session` or `system`) |
| `--dry-run` | Print bwrap command without executing |
| `--verbose` | Show resolved config (debug level) |

## Configuration

Place `.claude/wrap.toml` in any parent directory. claudewrap walks up from the current directory and merges all configs found (permissions are OR-merged; SSH keys are unioned across scopes).

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

[ssh]
agent = true         # pass through host SSH agent
# Key fingerprints to allow — find yours with: ssh-add -l -E sha256
keys = ["SHA256:abc123..."]
```

## License

MIT
