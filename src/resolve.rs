use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::Cli;
use crate::config::{DbusMode, LocatedConfig, SshConfig, WrapConfig};

/// Fully resolved configuration ready for sandbox building.
#[derive(Debug)]
pub struct ResolvedConfig {
    pub scope_id: Option<String>,
    pub write_paths: Vec<PathBuf>,
    pub wayland: bool,
    pub pipewire: bool,
    pub dbus: DbusMode,
    pub ssh: SshConfig,
    pub ssh_allow_all: bool,
    pub extra_ssh_hosts: Vec<String>,
    pub command: String,
    pub cmd_args: Vec<String>,
    pub cwd: PathBuf,
    pub dry_run: bool,
}

/// Walk from `start` up to `/`, collecting all `.claude/wrap.toml` files.
/// Returns configs ordered from innermost (closest to start) to outermost.
pub fn discover_configs(start: &Path) -> Result<Vec<LocatedConfig>> {
    let mut configs = Vec::new();
    let mut dir = start.to_path_buf();
    loop {
        let wrap_path = dir.join(".claude").join("wrap.toml");
        if wrap_path.is_file() {
            let content = std::fs::read_to_string(&wrap_path)
                .with_context(|| format!("reading {}", wrap_path.display()))?;
            let config: WrapConfig = toml::from_str(&content)
                .with_context(|| format!("parsing {}", wrap_path.display()))?;
            configs.push(LocatedConfig {
                base_dir: dir.clone(),
                config,
            });
        }
        if !dir.pop() {
            break;
        }
    }
    Ok(configs)
}

/// Resolve CLI arguments + discovered configs into a final config.
pub fn resolve(cli: &Cli) -> Result<ResolvedConfig> {
    let cwd = std::env::current_dir().context("getting current directory")?;
    let configs = discover_configs(&cwd)?;

    // Select scope
    let selected: Option<&LocatedConfig> = if let Some(ref scope_id) = cli.scope {
        let found = configs
            .iter()
            .find(|c| c.config.scope.id.as_deref() == Some(scope_id.as_str()));
        match found {
            Some(c) => Some(c),
            None => bail!("scope '{scope_id}' not found in any .claude/wrap.toml"),
        }
    } else {
        // Use innermost scope with default = true; fall back to none
        configs.iter().find(|c| c.config.scope.default)
    };

    // Merge write paths: selected scope + all ancestor scopes
    let mut write_paths: Vec<PathBuf> = Vec::new();

    if let Some(sel) = selected {
        // Find the index of the selected config
        let sel_idx = configs
            .iter()
            .position(|c| std::ptr::eq(c, sel))
            .unwrap();

        // Add write paths from selected scope and all ancestors (higher indices = further up)
        for located in &configs[sel_idx..] {
            for w in &located.config.filesystem.write {
                let resolved = resolve_path(&located.base_dir, w);
                write_paths.push(resolved);
            }
        }
    }

    // Auto-detect git repo / worktree and grant write access
    if let Some(git_paths) = detect_git_repo(&cwd) {
        write_paths.push(git_paths.work_tree);
        if let Some(common) = git_paths.common_git_dir {
            write_paths.push(common);
        }
    }

    // Add ad-hoc --write paths (resolved relative to cwd)
    for w in &cli.write {
        let resolved = if w.is_absolute() {
            w.clone()
        } else {
            cwd.join(w)
        };
        write_paths.push(resolved);
    }

    // Socket settings from selected scope (not inherited), with CLI overrides
    let (cfg_wayland, cfg_pipewire, cfg_dbus, cfg_ssh) = selected
        .map(|s| {
            (
                s.config.sockets.wayland,
                s.config.sockets.pipewire,
                s.config.sockets.dbus.clone(),
                s.config.ssh.clone(),
            )
        })
        .unwrap_or_default();

    let wayland = if cli.wayland {
        true
    } else if cli.no_wayland {
        false
    } else {
        cfg_wayland
    };

    let pipewire = if cli.pipewire { true } else { cfg_pipewire };

    let dbus = if let Some(ref mode) = cli.dbus {
        match mode.as_str() {
            "session" => DbusMode::Session,
            "system" => DbusMode::System,
            other => bail!("invalid --dbus mode: {other}"),
        }
    } else {
        cfg_dbus
    };

    let command = cli
        .exec
        .clone()
        .unwrap_or_else(|| "claude".to_string());

    Ok(ResolvedConfig {
        scope_id: selected.and_then(|s| s.config.scope.id.clone()),
        write_paths,
        wayland,
        pipewire,
        dbus,
        ssh: cfg_ssh,
        ssh_allow_all: cli.ssh_allow_all,
        extra_ssh_hosts: cli.ssh_allow_hosts.clone(),
        command,
        cmd_args: cli.cmd_args.clone(),
        cwd,
        dry_run: cli.dry_run,
    })
}

struct GitPaths {
    /// The worktree root (or repo root if not a worktree)
    work_tree: PathBuf,
    /// The common .git dir (only set for worktrees, where it differs from work_tree/.git)
    common_git_dir: Option<PathBuf>,
}

/// Detect git repo root and, for worktrees, the common git dir.
fn detect_git_repo(cwd: &Path) -> Option<GitPaths> {
    use std::process::Command;

    let toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !toplevel.status.success() {
        return None;
    }
    let work_tree = PathBuf::from(String::from_utf8_lossy(&toplevel.stdout).trim());

    // Check for worktree: --git-common-dir returns the main repo's .git dir
    let common = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;
    let common_git_dir = if common.status.success() {
        let dir = String::from_utf8_lossy(&common.stdout).trim().to_string();
        let dir_path = if Path::new(&dir).is_absolute() {
            PathBuf::from(&dir)
        } else {
            work_tree.join(&dir)
        };
        // Canonicalize and get the parent — if it differs from work_tree, it's a worktree
        let canonical = std::fs::canonicalize(&dir_path).ok()?;
        // The common git dir is e.g. /path/to/repo.git — mount the whole thing
        // so that worktree refs, packed-refs, objects etc. are writable
        if !canonical.starts_with(&work_tree) {
            Some(canonical)
        } else {
            None
        }
    } else {
        None
    };

    Some(GitPaths {
        work_tree,
        common_git_dir,
    })
}

fn resolve_path(base_dir: &Path, path_str: &str) -> PathBuf {
    let p = Path::new(path_str);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}
