//! Strict local remote-profile parsing and pure OpenSSH launch planning.

use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::default_config_dir;

const REMOTE_SCHEMA_VERSION: u8 = 1;
const MAX_REMOTE_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_REMOTE_DOCUMENT_BYTES_U64: u64 = 64 * 1024;
const MAX_REMOTE_PROFILES: usize = 64;
const MAX_HOST_BYTES: usize = 255;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_USER_BYTES: usize = 64;
const MAX_IDENTITY_FILES: usize = 8;
const MIN_CONNECT_TIMEOUT_SECONDS: u16 = 1;
const MAX_CONNECT_TIMEOUT_SECONDS: u16 = 300;
const DEFAULT_CONNECT_TIMEOUT_SECONDS: u16 = 15;
const DEFAULT_REMOTE_EXECUTABLE: &str = "/usr/bin/splinterm";

fn default_remote_executable() -> String {
    DEFAULT_REMOTE_EXECUTABLE.to_owned()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteDocument {
    version: u8,
    #[serde(default)]
    remotes: BTreeMap<String, RawRemoteProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRemoteProfile {
    host: String,
    user: Option<String>,
    port: Option<u16>,
    #[serde(default = "default_remote_executable")]
    executable: String,
    #[serde(default)]
    identity_files: Vec<String>,
    known_hosts_file: Option<String>,
    #[serde(default = "default_connect_timeout_seconds")]
    connect_timeout_seconds: u16,
}

const fn default_connect_timeout_seconds() -> u16 {
    DEFAULT_CONNECT_TIMEOUT_SECONDS
}

/// One validated named OpenSSH destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteProfile {
    name: String,
    host: String,
    user: Option<String>,
    port: Option<u16>,
    identity_files: Vec<PathBuf>,
    executable: String,
    known_hosts_file: Option<PathBuf>,
    connect_timeout_seconds: u16,
}

impl RemoteProfile {
    /// Returns the stable local profile name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the host or OpenSSH host alias.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the optional remote account name.
    #[must_use]
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// Returns the optional explicit SSH port.
    #[must_use]
    pub const fn port(&self) -> Option<u16> {
        self.port
    }

    /// Returns the explicitly configured local identity files.
    #[must_use]
    pub fn identity_files(&self) -> &[PathBuf] {
        &self.identity_files
    }

    /// Returns the optional explicit local known-hosts file.
    #[must_use]
    pub fn known_hosts_file(&self) -> Option<&Path> {
        self.known_hosts_file.as_deref()
    }

    /// Returns the bounded connection timeout.
    #[must_use]
    pub const fn connect_timeout_seconds(&self) -> u16 {
        self.connect_timeout_seconds
    }

    /// Constructs the fixed, non-shell OpenSSH process plan.
    #[must_use]
    pub fn ssh_plan(&self) -> SshLaunchPlan {
        let mut arguments = vec![
            OsString::from("-T"),
            OsString::from("-o"),
            OsString::from("StrictHostKeyChecking=yes"),
            OsString::from("-o"),
            OsString::from("ClearAllForwardings=yes"),
            OsString::from("-o"),
            OsString::from("PermitLocalCommand=no"),
            OsString::from("-o"),
            OsString::from("RequestTTY=no"),
            OsString::from("-o"),
            OsString::from("EscapeChar=none"),
            OsString::from("-o"),
            OsString::from("RemoteCommand=none"),
            OsString::from("-o"),
            OsString::from("StdinNull=no"),
            OsString::from("-o"),
            OsString::from("SessionType=default"),
            OsString::from("-o"),
            OsString::from(format!("ConnectTimeout={}", self.connect_timeout_seconds)),
        ];
        if let Some(port) = self.port {
            arguments.push(OsString::from("-p"));
            arguments.push(OsString::from(port.to_string()));
        }
        if let Some(user) = &self.user {
            arguments.push(OsString::from("-l"));
            arguments.push(OsString::from(user));
        }
        for identity in &self.identity_files {
            arguments.push(OsString::from("-i"));
            arguments.push(identity.as_os_str().to_owned());
        }
        if let Some(known_hosts) = &self.known_hosts_file {
            arguments.push(OsString::from("-o"));
            arguments.push(OsString::from(format!(
                "UserKnownHostsFile={}",
                known_hosts.display()
            )));
        }
        arguments.push(OsString::from(&self.host));
        // OpenSSH sends this string to the remote login shell. The executable
        // is a strictly validated absolute path, never a command or shell text.
        arguments.push(OsString::from(format!(
            "{} relay --graphical-stdio",
            self.executable
        )));
        SshLaunchPlan {
            program: OsString::from("ssh"),
            arguments,
        }
    }
}

/// A process launch description containing no credential material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshLaunchPlan {
    program: OsString,
    arguments: Vec<OsString>,
}

impl SshLaunchPlan {
    /// Returns the fixed OpenSSH executable name.
    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// Returns structured argv in process order.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

/// A deterministic catalog of validated named remotes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteCatalog {
    profiles: BTreeMap<String, RemoteProfile>,
}

impl RemoteCatalog {
    /// Loads `remotes.toml`, returning an empty catalog when the default file is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when an explicit file cannot be read or validated.
    pub fn load_default() -> Result<Self> {
        let override_path = env::var_os("SPLINTERM_REMOTES");
        let path = override_path
            .clone()
            .map_or_else(|| default_config_dir().join("remotes.toml"), PathBuf::from);
        match fs::symlink_metadata(&path) {
            Ok(_) => Self::load(&path),
            Err(error) if error.kind() == ErrorKind::NotFound && override_path.is_none() => {
                Ok(Self::default())
            }
            Err(error) => {
                Err(error).with_context(|| format!("inspect remote profiles at {}", path.display()))
            }
        }
    }

    /// Loads one explicit profile document.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or its contents are invalid.
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("read remote profiles from {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("inspect remote profiles at {}", path.display()))?;
        if !metadata.is_file() {
            bail!(
                "remote profile path {} is not a regular file",
                path.display()
            );
        }
        if metadata.len() > MAX_REMOTE_DOCUMENT_BYTES_U64 {
            bail!("remote profile document exceeds {MAX_REMOTE_DOCUMENT_BYTES} bytes");
        }
        let mut bytes = Vec::new();
        file.take(MAX_REMOTE_DOCUMENT_BYTES_U64 + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("read remote profiles from {}", path.display()))?;
        if bytes.len() > MAX_REMOTE_DOCUMENT_BYTES {
            bail!("remote profile document exceeds {MAX_REMOTE_DOCUMENT_BYTES} bytes");
        }
        let text = String::from_utf8(bytes).context("remote profile document is not UTF-8")?;
        let home = env::var_os("HOME").map(PathBuf::from);
        Self::parse(&text, home.as_deref())
            .with_context(|| format!("parse remote profiles from {}", path.display()))
    }

    /// Parses and validates one profile document without launching a process.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported schema or any invalid profile value.
    pub fn parse(text: &str, home: Option<&Path>) -> Result<Self> {
        let document: RemoteDocument =
            toml::from_str(text).context("remote profile TOML is invalid")?;
        if document.version != REMOTE_SCHEMA_VERSION {
            bail!(
                "unsupported remote profile schema {}; expected {REMOTE_SCHEMA_VERSION}",
                document.version
            );
        }
        if document.remotes.len() > MAX_REMOTE_PROFILES {
            bail!("remote profile count exceeds {MAX_REMOTE_PROFILES}");
        }
        let mut profiles = BTreeMap::new();
        for (name, raw) in document.remotes {
            validate_profile_name(&name)?;
            validate_host(&raw.host).with_context(|| format!("remote profile {name}"))?;
            validate_remote_executable(&raw.executable)
                .with_context(|| format!("remote profile {name}"))?;
            if let Some(user) = &raw.user {
                validate_user(user).with_context(|| format!("remote profile {name}"))?;
            }
            if raw.port == Some(0) {
                bail!("remote profile {name} has invalid port 0");
            }
            if !(MIN_CONNECT_TIMEOUT_SECONDS..=MAX_CONNECT_TIMEOUT_SECONDS)
                .contains(&raw.connect_timeout_seconds)
            {
                bail!(
                    "remote profile {name} connect timeout must be {MIN_CONNECT_TIMEOUT_SECONDS}..={MAX_CONNECT_TIMEOUT_SECONDS} seconds"
                );
            }
            if raw.identity_files.len() > MAX_IDENTITY_FILES {
                bail!("remote profile {name} has too many identity files");
            }
            let identity_files = raw
                .identity_files
                .iter()
                .map(|path| {
                    resolve_readable_file(path, home)
                        .with_context(|| format!("remote profile {name} identity file"))
                })
                .collect::<Result<Vec<_>>>()?;
            let known_hosts_file = raw
                .known_hosts_file
                .as_deref()
                .map(|path| {
                    resolve_readable_file(path, home)
                        .with_context(|| format!("remote profile {name} known-hosts file"))
                })
                .transpose()?;
            profiles.insert(
                name.clone(),
                RemoteProfile {
                    name,
                    host: raw.host,
                    executable: raw.executable,
                    user: raw.user,
                    port: raw.port,
                    identity_files,
                    known_hosts_file,
                    connect_timeout_seconds: raw.connect_timeout_seconds,
                },
            );
        }
        Ok(Self { profiles })
    }

    /// Returns profiles in stable name order.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &RemoteProfile> {
        self.profiles.values()
    }

    /// Resolves one exact profile name.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact profile name is not configured.
    pub fn get(&self, name: &str) -> Result<&RemoteProfile> {
        self.profiles
            .get(name)
            .with_context(|| format!("remote profile {name:?} is not configured"))
    }

    /// Returns whether the catalog is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}

/// Returns whether `name` satisfies the profile-name rule, so it is safe to
/// display even when no such profile is configured.
#[must_use]
pub fn is_valid_profile_name(name: &str) -> bool {
    validate_profile_name(name).is_ok()
}

fn has_unsafe_text(value: &str) -> bool {
    value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
    })
}

fn validate_profile_name(name: &str) -> Result<()> {
    // The protocol owns the rule, so a consent origin and a configured
    // profile can never disagree on what a usable name is.
    if !splinterm_protocol::usable_remote_profile_name(name) {
        bail!("remote profile name {name:?} is invalid");
    }
    Ok(())
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || host.len() > MAX_HOST_BYTES
        || host.starts_with('-')
        || has_unsafe_text(host)
        || !host.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-' | b':' | b'[' | b']' | b'%')
        })
    {
        bail!("SSH host or alias {host:?} is invalid");
    }
    Ok(())
}

fn validate_user(user: &str) -> Result<()> {
    if user.is_empty()
        || user.len() > MAX_USER_BYTES
        || user.starts_with('-')
        || has_unsafe_text(user)
        || !user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("SSH user {user:?} is invalid");
    }
    Ok(())
}

fn validate_remote_executable(value: &str) -> Result<()> {
    // This is a remote path: do not resolve it on the client or expand HOME.
    // A conservative ASCII allowlist makes it one literal POSIX-shell word.
    if !value.starts_with('/')
        || value.len() > MAX_PATH_BYTES
        || value
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || part == "." || part == "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
    {
        bail!(
            "remote executable must be an absolute path with only ASCII letters, digits, /, ., _, or - and no empty or dot components"
        );
    }
    Ok(())
}

fn resolve_readable_file(value: &str, home: Option<&Path>) -> Result<PathBuf> {
    if value.is_empty()
        || value.len() > MAX_PATH_BYTES
        || has_unsafe_text(value)
        || value.chars().any(char::is_whitespace)
    {
        bail!("configured path is empty, ambiguous, or contains unsafe text");
    }
    let path = if value == "~" {
        home.context("HOME is unavailable for ~ path expansion")?
            .to_owned()
    } else if let Some(suffix) = value.strip_prefix("~/") {
        home.context("HOME is unavailable for ~ path expansion")?
            .join(suffix)
    } else {
        if value.contains('~') {
            bail!("~ is only supported as the first path component");
        }
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        bail!("configured path must be absolute or start with ~/");
    }
    let metadata = fs::metadata(&path)
        .with_context(|| format!("cannot inspect configured file {}", path.display()))?;
    if !metadata.is_file() {
        bail!("configured path {} is not a regular file", path.display());
    }
    File::open(&path)
        .with_context(|| format!("configured file {} is not readable", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn fixture_home() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let home =
            env::temp_dir().join(format!("splinterm-remotes-{}-{nonce}", std::process::id()));
        fs::create_dir_all(home.join(".ssh")).unwrap();
        fs::write(home.join(".ssh/id_ed25519"), "fixture key").unwrap();
        fs::write(home.join(".ssh/known_hosts"), "fixture host").unwrap();
        home
    }

    #[test]
    fn strict_profile_parses_and_builds_fixed_structured_ssh_argv() {
        let home = fixture_home();
        let catalog = RemoteCatalog::parse(
            r#"
version = 1

[remotes.wintermute]
host = "wintermute"
user = "operator"
port = 2222
identity_files = ["~/.ssh/id_ed25519"]
known_hosts_file = "~/.ssh/known_hosts"
connect_timeout_seconds = 23
"#,
            Some(&home),
        )
        .unwrap();
        let profile = catalog.get("wintermute").unwrap();
        assert_eq!(profile.host(), "wintermute");
        assert_eq!(profile.user(), Some("operator"));
        assert_eq!(profile.port(), Some(2222));
        assert_eq!(profile.connect_timeout_seconds(), 23);
        let plan = profile.ssh_plan();
        assert_eq!(plan.program(), OsStr::new("ssh"));
        let args = plan
            .arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args.last().unwrap(),
            "/usr/bin/splinterm relay --graphical-stdio"
        );
        assert_eq!(args[args.len() - 2], "wintermute");
        assert!(args.windows(2).any(|pair| pair == ["-l", "operator"]));
        assert!(args.windows(2).any(|pair| pair == ["-p", "2222"]));
        assert!(
            args.iter()
                .any(|argument| argument == "StrictHostKeyChecking=yes")
        );
        assert!(
            args.iter()
                .any(|argument| argument == "ClearAllForwardings=yes")
        );
        assert!(
            args.iter()
                .any(|argument| argument == "PermitLocalCommand=no")
        );
        assert!(args.iter().any(|argument| argument == "RemoteCommand=none"));
        assert!(!args.iter().any(|argument| argument.contains("fixture key")));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn nixos_remote_executable_is_literal_and_need_not_exist_locally() {
        for executable in [
            "/run/current-system/sw/bin/splinterm",
            "/nix/store/0123456789abcdef-splinterm/bin/splinterm",
            "/home/operator/.nix-profile/bin/splinterm",
        ] {
            let catalog = RemoteCatalog::parse(
                &format!(
                    "version = 1\n[remotes.nixos]\nhost = \"nixos\"\nexecutable = {executable:?}\n"
                ),
                None,
            )
            .unwrap();
            let plan = catalog.get("nixos").unwrap().ssh_plan();
            assert_eq!(
                plan.arguments().last().unwrap(),
                &OsString::from(format!("{executable} relay --graphical-stdio"))
            );
        }
    }

    #[test]
    fn remote_executable_rejects_shell_syntax_and_ambiguous_paths() {
        for executable in [
            "",
            "splinterm",
            "~/bin/splinterm",
            "/",
            "/bin/",
            "//bin/splinterm",
            "/bin/../splinterm",
            "/bin/./splinterm",
            "/bin/splinterm --flag",
            "/bin/splinterm;id",
            "/bin/$(id)",
            "/bin/`id`",
            "/bin/$HOME",
            "/bin/'splinterm'",
            "/bin/\"splinterm\"",
            "/bin/splinterm\n",
            "/bin/*",
            "/bin/splinterm\\x",
            "/bin/splinterm\u{202e}",
        ] {
            assert!(
                validate_remote_executable(executable).is_err(),
                "{executable:?}"
            );
        }
        assert!(validate_remote_executable(&format!("/{}", "a".repeat(MAX_PATH_BYTES))).is_err());
        assert!(
            RemoteCatalog::parse(
                "version = 1\n[remotes.bad]\nhost = \"safe\"\nexecutable = \"/bin/splinterm;id\"\n",
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn profiles_are_sorted_and_defaults_do_not_invent_credentials() {
        let catalog = RemoteCatalog::parse(
            r#"
version = 1
[remotes.zeta]
host = "zeta.example"
[remotes.alpha]
host = "alpha.example"
"#,
            None,
        )
        .unwrap();
        assert_eq!(
            catalog.iter().map(RemoteProfile::name).collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        let alpha = catalog.get("alpha").unwrap();
        assert_eq!(alpha.user(), None);
        assert_eq!(alpha.port(), None);
        assert!(alpha.identity_files().is_empty());
        assert_eq!(
            alpha.connect_timeout_seconds(),
            DEFAULT_CONNECT_TIMEOUT_SECONDS
        );
    }

    #[test]
    fn unknown_fields_versions_and_unsafe_tokens_fail_closed() {
        for document in [
            "version = 2\n",
            "version = 1\nunknown = true\n",
            "version = 1\n[remotes.bad]\nhost = \"-oProxyCommand=x\"\n",
            "version = 1\n[remotes.bad]\nhost = \"host name\"\n",
            "version = 1\n[remotes.'bad name']\nhost = \"safe\"\n",
            "version = 1\n[remotes.bad]\nhost = \"safe\"\nuser = \"-root\"\n",
            "version = 1\n[remotes.bad]\nhost = \"safe\"\nconnect_timeout_seconds = 0\n",
        ] {
            assert!(RemoteCatalog::parse(document, None).is_err(), "{document}");
        }
    }

    #[test]
    fn document_and_path_sizes_are_bounded_before_use() {
        let directory = fixture_home();
        let document_path = directory.join("oversized.toml");
        fs::write(&document_path, "x".repeat(MAX_REMOTE_DOCUMENT_BYTES + 1)).unwrap();
        let document_error = RemoteCatalog::load(&document_path).unwrap_err();
        assert!(format!("{document_error:#}").contains("exceeds"));

        let oversized_path = format!("/{}", "x".repeat(MAX_PATH_BYTES));
        let path_error = RemoteCatalog::parse(
            &format!(
                "version = 1\n[remotes.bad]\nhost = \"safe\"\nidentity_files = [{oversized_path:?}]\n"
            ),
            Some(&directory),
        )
        .unwrap_err();
        assert!(format!("{path_error:#}").contains("ambiguous"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn explicit_paths_are_absolute_expanded_and_readable() {
        let home = fixture_home();
        let relative = RemoteCatalog::parse(
            "version = 1\n[remotes.bad]\nhost = \"safe\"\nidentity_files = [\"id\"]\n",
            Some(&home),
        )
        .unwrap_err();
        assert!(format!("{relative:#}").contains("absolute"));
        let missing = RemoteCatalog::parse(
            "version = 1\n[remotes.bad]\nhost = \"safe\"\nidentity_files = [\"~/.ssh/missing\"]\n",
            Some(&home),
        )
        .unwrap_err();
        assert!(format!("{missing:#}").contains("cannot inspect"));
        let ambiguous = RemoteCatalog::parse(
            "version = 1\n[remotes.bad]\nhost = \"safe\"\nknown_hosts_file = \"/tmp/hosts alternate\"\n",
            Some(&home),
        )
        .unwrap_err();
        assert!(format!("{ambiguous:#}").contains("ambiguous"));
        fs::remove_dir_all(home).unwrap();
    }
}
