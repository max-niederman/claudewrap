use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::Cli;
use crate::config::{DbusMode, LocatedConfig, WrapConfig};

/// Fully resolved configuration ready for sandbox building.
#[derive(Debug)]
pub struct ResolvedConfig {
    pub active_scopes: Vec<String>,
    pub write_paths: Vec<PathBuf>,
    /// Explicit read-only paths (ro-bound into the sandbox).
    pub read_paths: Vec<PathBuf>,
    /// Paths to hide inside the sandbox (applied after all bind-mounts).
    /// Already filtered: paths with an exact match in write/read are dropped.
    pub mask_paths: Vec<PathBuf>,
    pub wayland: bool,
    pub pipewire: bool,
    pub docker: bool,
    pub dbus: DbusMode,
    pub command: String,
    pub cmd_args: Vec<String>,
    pub cwd: PathBuf,
    pub dry_run: bool,
    /// Whether to pass through the host SSH agent
    pub ssh_agent: bool,
    /// SSH key fingerprints to validate in host agent
    pub ssh_keys: Vec<String>,
    /// Paths to discovered wrap.toml files (to be mounted read-only)
    pub config_files: Vec<PathBuf>,
}

/// Walk from `start` up to `/`, collecting all `.claude/wrap.toml` files.
/// Returns configs ordered from innermost (closest to start) to outermost.
pub fn discover_configs(start: &Path) -> Result<Vec<LocatedConfig>> {
    let mut configs = Vec::new();
    let mut dir = start.to_path_buf();
    loop {
        let wrap_path = dir.join(".claude").join("wrap.toml");
        // Only load config files that are regular files — skip symlinks to
        // prevent a sandboxed process from replacing a config with a symlink
        // that points to an attacker-controlled file on the next invocation.
        let is_regular = wrap_path
            .symlink_metadata()
            .map(|m| m.is_file())
            .unwrap_or(false);
        if is_regular {
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

    // Collect active scopes: all default=true scopes, plus any explicitly requested via -s
    let mut active: Vec<&LocatedConfig> = configs
        .iter()
        .filter(|c| c.config.scope.default)
        .collect();

    for scope_id in &cli.scope {
        let found = configs
            .iter()
            .find(|c| c.config.scope.id.as_deref() == Some(scope_id.as_str()));
        match found {
            Some(c) => {
                if !active.iter().any(|a| std::ptr::eq(*a, c)) {
                    active.push(c);
                }
            }
            None => bail!("scope '{scope_id}' not found in any .claude/wrap.toml"),
        }
    }

    let active_scopes: Vec<String> = active
        .iter()
        .filter_map(|s| s.config.scope.id.clone())
        .collect();

    // OR-merge all active scopes: write paths, sockets — permissions only expand
    let mut write_paths: Vec<PathBuf> = Vec::new();
    let (mut cfg_wayland, mut cfg_pipewire, mut cfg_docker, mut cfg_dbus) =
        <(bool, bool, bool, DbusMode)>::default();

    for located in &active {
        for w in &located.config.filesystem.write {
            write_paths.push(resolve_path(&located.base_dir, w));
        }
        cfg_wayland |= located.config.sockets.wayland;
        cfg_pipewire |= located.config.sockets.pipewire;
        cfg_docker |= located.config.sockets.docker;
        cfg_dbus = cfg_dbus.merge(&located.config.sockets.dbus);
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

    // Read paths: explicit ro-bind entries, used to re-expose a default mask.
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let mut read_paths: Vec<PathBuf> = Vec::new();
    for located in &active {
        for r in &located.config.filesystem.read {
            read_paths.push(resolve_mask_path(&located.base_dir, r, home.as_deref()));
        }
    }
    for r in &cli.read {
        let s = r.to_string_lossy();
        let expanded = expand_tilde(&s, home.as_deref());
        let resolved = if expanded.is_absolute() {
            expanded
        } else {
            cwd.join(expanded)
        };
        read_paths.push(resolved);
    }

    // Mask paths: default credential stores + any user-configured paths.
    // Default entries are cancelled by an exact-path match in write or read.
    let mut default_masks: Vec<PathBuf> = Vec::new();
    if let Some(ref h) = home {
        default_masks.push(h.join(".config/gcloud"));
        default_masks.push(h.join(".aws"));
    }
    let exempt: std::collections::HashSet<PathBuf> = write_paths
        .iter()
        .chain(read_paths.iter())
        .map(|p| normalize_for_compare(p))
        .collect();
    let mut mask_paths: Vec<PathBuf> = default_masks
        .into_iter()
        .filter(|p| !exempt.contains(&normalize_for_compare(p)))
        .collect();
    for located in &active {
        for m in &located.config.filesystem.mask {
            mask_paths.push(resolve_mask_path(&located.base_dir, m, home.as_deref()));
        }
    }
    for m in &cli.mask {
        let s = m.to_string_lossy();
        let expanded = expand_tilde(&s, home.as_deref());
        let resolved = if expanded.is_absolute() {
            expanded
        } else {
            cwd.join(expanded)
        };
        mask_paths.push(resolved);
    }

    let wayland = if cli.wayland {
        true
    } else if cli.no_wayland {
        false
    } else {
        cfg_wayland
    };

    let pipewire = if cli.pipewire { true } else { cfg_pipewire };

    let docker = if cli.docker { true } else { cfg_docker };

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

    // SSH: OR-merge agent flag across scopes, union keys; CLI --ssh-key adds to the set
    let mut ssh_agent = active.iter().any(|c| c.config.ssh.agent);
    let mut ssh_keys: Vec<String> = active
        .iter()
        .flat_map(|c| c.config.ssh.keys.iter().cloned())
        .collect();
    for k in &cli.ssh_key {
        if !ssh_keys.contains(k) {
            ssh_keys.push(k.clone());
        }
        // --ssh-key implies agent
        ssh_agent = true;
    }
    if cli.no_ssh_agent {
        ssh_agent = false;
    }

    let config_files: Vec<PathBuf> = configs
        .iter()
        .map(|c| c.base_dir.join(".claude").join("wrap.toml"))
        .collect();

    Ok(ResolvedConfig {
        active_scopes,
        write_paths,
        read_paths,
        mask_paths,
        wayland,
        pipewire,
        docker,
        dbus,
        command,
        cmd_args: cli.cmd_args.clone(),
        cwd,
        dry_run: cli.dry_run,
        ssh_agent,
        ssh_keys,
        config_files,
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

/// Collapse `.` / `..` and drop trailing empty components so two path
/// spellings of the same location compare equal. Does not follow symlinks.
fn normalize_for_compare(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = Vec::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// Expand a leading `~/` or bare `~` to $HOME. Returns input unchanged if
/// HOME isn't set or the string doesn't start with `~`.
fn expand_tilde(s: &str, home: Option<&Path>) -> PathBuf {
    match (home, s) {
        (Some(h), "~") => h.to_path_buf(),
        (Some(h), _) if s.starts_with("~/") => h.join(&s[2..]),
        _ => PathBuf::from(s),
    }
}

/// Resolve a mask path string: tilde first, then absolute-or-relative-to-base.
fn resolve_mask_path(base_dir: &Path, path_str: &str, home: Option<&Path>) -> PathBuf {
    let expanded = expand_tilde(path_str, home);
    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}
