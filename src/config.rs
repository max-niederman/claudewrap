use serde::Deserialize;
use std::path::PathBuf;

/// Root structure of a `.claude/wrap.toml` file.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct WrapConfig {
    pub scope: ScopeConfig,
    pub filesystem: FilesystemConfig,
    pub sockets: SocketsConfig,
    pub ssh: SshConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SshConfig {
    /// Whether to pass through the host SSH agent (default: false)
    pub agent: bool,
    /// Key fingerprints to validate and allow (e.g. ["SHA256:..."])
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ScopeConfig {
    pub id: Option<String>,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct FilesystemConfig {
    #[serde(default)]
    pub write: Vec<String>,
    /// Explicit read-only paths. Mostly a no-op (the base is ro-bind /), but
    /// an exact match cancels a default mask.
    #[serde(default)]
    pub read: Vec<String>,
    /// Paths to hide from the sandbox. Directories become tmpfs, files
    /// are shadowed with /dev/null. Leading `~/` is expanded to $HOME.
    /// Default masks (~/.config/gcloud, ~/.aws) are cancelled when the exact
    /// path appears in `write` or `read`.
    #[serde(default)]
    pub mask: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SocketsConfig {
    #[serde(default)]
    pub wayland: bool,
    #[serde(default)]
    pub pipewire: bool,
    #[serde(default)]
    pub docker: bool,
    #[serde(default, deserialize_with = "deserialize_dbus")]
    pub dbus: DbusMode,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum DbusMode {
    #[default]
    Disabled,
    Session,
    System,
}

fn deserialize_dbus<'de, D: serde::Deserializer<'de>>(d: D) -> Result<DbusMode, D::Error> {
    use serde::de;

    struct DbusVisitor;
    impl<'de> de::Visitor<'de> for DbusVisitor {
        type Value = DbusMode;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str(r#""session", "system", or false"#)
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> Result<DbusMode, E> {
            if v {
                Err(E::custom("dbus = true is not valid; use \"session\" or \"system\""))
            } else {
                Ok(DbusMode::Disabled)
            }
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<DbusMode, E> {
            match v {
                "session" => Ok(DbusMode::Session),
                "system" => Ok(DbusMode::System),
                other => Err(E::custom(format!("unknown dbus mode: {other}"))),
            }
        }
    }
    d.deserialize_any(DbusVisitor)
}

impl DbusMode {
    /// OR-merge: most permissive wins. System > Session > Disabled.
    pub fn merge(&self, other: &DbusMode) -> DbusMode {
        match (self, other) {
            (DbusMode::System, _) | (_, DbusMode::System) => DbusMode::System,
            (DbusMode::Session, _) | (_, DbusMode::Session) => DbusMode::Session,
            _ => DbusMode::Disabled,
        }
    }
}

/// A parsed config together with the directory it was found in.
#[derive(Debug, Clone)]
pub struct LocatedConfig {
    /// The parent directory of the `.claude/` dir containing wrap.toml
    pub base_dir: PathBuf,
    pub config: WrapConfig,
}
