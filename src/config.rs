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
pub struct ScopeConfig {
    pub id: Option<String>,
    #[serde(default = "default_true")]
    pub default: bool,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct FilesystemConfig {
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SocketsConfig {
    #[serde(default)]
    pub wayland: bool,
    #[serde(default)]
    pub pipewire: bool,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SshConfig {
    pub agent: bool,
    #[serde(rename = "allow-signing")]
    pub allow_signing: bool,
    #[serde(rename = "allow-ssh")]
    pub allow_ssh: bool,
    #[serde(rename = "allow-hosts")]
    pub allow_hosts: Vec<String>,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            agent: true,
            allow_signing: true,
            allow_ssh: false,
            allow_hosts: vec!["github.com".into(), "gitlab.com".into()],
        }
    }
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

impl SshConfig {
    /// OR-merge: union of permissions across scopes.
    pub fn merge(&self, other: &SshConfig) -> SshConfig {
        let mut hosts = self.allow_hosts.clone();
        for h in &other.allow_hosts {
            if !hosts.contains(h) {
                hosts.push(h.clone());
            }
        }
        SshConfig {
            agent: self.agent || other.agent,
            allow_signing: self.allow_signing || other.allow_signing,
            allow_ssh: self.allow_ssh || other.allow_ssh,
            allow_hosts: hosts,
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
