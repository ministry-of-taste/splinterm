//! Race-resistant snapshots of a local peer's executable.
//!
//! # Identity invariant
//!
//! Every identity field except `path` (device, inode, owner, mode, size, and
//! digest) is read through one open descriptor on the executing file object,
//! obtained by opening the kernel's executable link, so it describes exactly
//! the bytes the process runs. `path` is accepted only as a binding, never as
//! the source of identity: it must be a bounded canonical absolute path, not
//! an unlinked one, and after hashing it must still resolve in this mount
//! namespace to the same device and inode as the open descriptor.
//!
//! An executable with no such path fails closed: a memfd image
//! (`/memfd:… (deleted)`), an unlinked or replaced file, a path changed while
//! hashing, or a path that names a different file here (another mount
//! namespace). Callers that need only device and inode, such as trusted-client
//! matching, read them from the opened link directly and need no path.

use std::{
    fs::{self, File, Metadata},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};

const MAX_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_EXECUTABLE_PATH_BYTES: usize = 4096;
const HASH_BUFFER_BYTES: usize = 64 * 1024;
const CURRENT_PROCESS_EXECUTABLE: &str = "/proc/self/exe";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableIdentity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub owner_uid: u32,
    pub mode: u32,
    pub size: u64,
    pub sha256: String,
}

impl ExecutableIdentity {
    /// Snapshots and hashes the exact executable currently held by `pid`,
    /// bound to the path its executable link names.
    ///
    /// # Errors
    ///
    /// Returns an error when procfs identity, path, metadata, size, or stable
    /// descriptor verification fails, including for a memfd-backed, unlinked,
    /// or replaced executable.
    pub fn from_pid(pid: u32) -> Result<Self> {
        snapshot_link(Path::new(&format!("/proc/{pid}/exe")))
            .with_context(|| format!("cannot snapshot peer executable for pid {pid}"))
    }

    /// Snapshots and hashes the executable running this process, bound to
    /// `installed`, a path the caller already holds as authority (such as the
    /// daemon's installed prefix) instead of the executable link's text.
    ///
    /// The same invariant holds: `installed` must still name the running file.
    ///
    /// # Errors
    ///
    /// Returns an error when `installed` is not a bounded canonical absolute
    /// path, the running image cannot be hashed stably, or `installed` does not
    /// name it.
    pub fn of_current_process_at(installed: &Path) -> Result<Self> {
        validate_canonical_path(installed)?;
        let mut file = open_executable(Path::new(CURRENT_PROCESS_EXECUTABLE))?;
        let (metadata, sha256) = hash_stable(&mut file, || Ok(()))?;
        bind_path(installed, &metadata)?;
        Ok(identity(installed.to_path_buf(), &metadata, sha256))
    }
}

fn snapshot_link(link: &Path) -> Result<ExecutableIdentity> {
    snapshot_link_with(link, || Ok(()))
}

fn snapshot_link_with(
    link: &Path,
    after_open: impl FnOnce() -> Result<()>,
) -> Result<ExecutableIdentity> {
    let target_before = fs::read_link(link).context("cannot resolve executable link")?;
    validate_canonical_path(&target_before)?;

    let mut file = open_executable(link)?;
    let (metadata, sha256) = hash_stable(&mut file, after_open)?;

    let target_after = fs::read_link(link).context("cannot re-resolve executable link")?;
    if target_after != target_before {
        bail!("executable link changed while hashing");
    }
    bind_path(&target_after, &metadata)?;
    Ok(identity(target_after, &metadata, sha256))
}

/// Opens the executing file object through the kernel's executable link,
/// which resolves to it even when its path no longer names it.
fn open_executable(link: &Path) -> Result<File> {
    let descriptor = rustix::fs::open(link, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .context("cannot open executable link")?;
    Ok(File::from(descriptor))
}

/// Hashes the open executable, requiring its metadata to be unchanged across
/// the read.
fn hash_stable(
    file: &mut File,
    after_open: impl FnOnce() -> Result<()>,
) -> Result<(Metadata, String)> {
    let metadata_before = file.metadata().context("cannot stat opened executable")?;
    validate_metadata(&metadata_before)?;

    after_open()?;
    let sha256 = hash_bounded(file, metadata_before.len())?;
    let metadata_after = file.metadata().context("cannot restat opened executable")?;
    if metadata_fingerprint(&metadata_before) != metadata_fingerprint(&metadata_after) {
        bail!("executable changed while hashing");
    }
    Ok((metadata_after, sha256))
}

/// Requires `path` to resolve, here and now, to the opened file.
fn bind_path(path: &Path, opened: &Metadata) -> Result<()> {
    let path_metadata = fs::metadata(path).context("executable path no longer resolves")?;
    if path_metadata.dev() != opened.dev() || path_metadata.ino() != opened.ino() {
        bail!("executable path no longer names the opened file");
    }
    Ok(())
}

fn identity(path: PathBuf, metadata: &Metadata, sha256: String) -> ExecutableIdentity {
    ExecutableIdentity {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        owner_uid: metadata.uid(),
        mode: metadata.mode(),
        size: metadata.len(),
        sha256,
    }
}

fn validate_canonical_path(path: &Path) -> Result<()> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_EXECUTABLE_PATH_BYTES || !path.is_absolute() {
        bail!("executable path is not a bounded absolute path");
    }
    if bytes.ends_with(b" (deleted)") {
        bail!("executable has been unlinked");
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        bail!("executable path is not canonical");
    }
    Ok(())
}

fn validate_metadata(metadata: &Metadata) -> Result<()> {
    if !metadata.is_file() {
        bail!("executable is not a regular file");
    }
    if metadata.len() == 0 || metadata.len() > MAX_EXECUTABLE_BYTES {
        bail!("executable size is outside the supported bound");
    }
    Ok(())
}

fn hash_bounded(file: &mut File, expected_size: u64) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES].into_boxed_slice();
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer).context("cannot read executable")?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count).expect("buffer length fits u64"))
            .context("executable byte count overflow")?;
        if total > MAX_EXECUTABLE_BYTES {
            bail!("executable exceeded the supported bound while hashing");
        }
        hasher.update(&buffer[..count]);
    }
    if total != expected_size {
        bail!("executable size changed while hashing");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn metadata_fingerprint(metadata: &Metadata) -> (u64, u64, u64, u32, u32, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.uid(),
        metadata.mode(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "splinterm-executable-identity-{}-{unique}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn snapshots_current_process_through_proc() {
        let identity = ExecutableIdentity::from_pid(std::process::id())
            .expect("snapshot current process executable");

        assert!(identity.path.is_absolute());
        assert!(identity.size > 0);
        assert_eq!(identity.sha256.len(), 64);
    }

    #[test]
    fn snapshots_exact_opened_regular_file() {
        let directory = TestDirectory::new();
        let executable = directory.0.join("client");
        let link = directory.0.join("exe");
        fs::write(&executable, b"abc").expect("write executable");
        symlink(&executable, &link).expect("create executable link");

        let identity = snapshot_link(&link).expect("snapshot executable");

        assert_eq!(identity.path, executable);
        assert_eq!(identity.size, 3);
        assert_eq!(
            identity.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rejects_path_replacement_after_descriptor_open() {
        let directory = TestDirectory::new();
        let executable = directory.0.join("client");
        let displaced = directory.0.join("client.old");
        let link = directory.0.join("exe");
        fs::write(&executable, b"authorized").expect("write executable");
        symlink(&executable, &link).expect("create executable link");

        let result = snapshot_link_with(&link, || {
            fs::rename(&executable, &displaced).context("displace executable")?;
            fs::write(&executable, b"replacement").context("write replacement")?;
            Ok(())
        });

        assert!(result.is_err());
    }

    #[test]
    fn rejects_in_place_mutation_after_descriptor_open() {
        let directory = TestDirectory::new();
        let executable = directory.0.join("client");
        let link = directory.0.join("exe");
        fs::write(&executable, b"authorized").expect("write executable");
        symlink(&executable, &link).expect("create executable link");

        let result = snapshot_link_with(&link, || {
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&executable)
                .context("open executable for mutation")?;
            file.write_all(b"changed").context("mutate executable")?;
            Ok(())
        });

        assert!(result.is_err());
    }

    #[test]
    fn rejects_relative_non_regular_and_oversized_targets() {
        let directory = TestDirectory::new();
        let relative_link = directory.0.join("relative");
        symlink("client", &relative_link).expect("create relative link");
        assert!(snapshot_link(&relative_link).is_err());

        let directory_link = directory.0.join("directory");
        symlink(&directory.0, &directory_link).expect("create directory link");
        assert!(snapshot_link(&directory_link).is_err());

        let oversized = directory.0.join("oversized");
        let oversized_link = directory.0.join("oversized-link");
        let file = File::create(&oversized).expect("create sparse file");
        file.set_len(MAX_EXECUTABLE_BYTES + 1)
            .expect("size sparse file");
        symlink(&oversized, &oversized_link).expect("create oversized link");
        assert!(snapshot_link(&oversized_link).is_err());
    }

    /// A child process the test started and owns, killed and reaped on drop.
    struct OwnedChild(std::process::Child);

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The program the tests copy and run under other names. Not `sleep`:
    /// coreutils may be one multicall binary that picks its tool by name,
    /// so a copy installed as `client` exits at once. A shell runs as any
    /// name.
    fn system_shell() -> &'static Path {
        ["/bin/sh", "/usr/bin/sh"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .expect("a shell")
    }

    /// Runs `program`, a copy of the system shell, until the test drops it.
    /// Without `BASH_ENV`, a bash run under another name reads no startup
    /// file.
    fn spawn_waiting(program: &Path) -> OwnedChild {
        // A sibling test thread's fork can briefly inherit the descriptor
        // that wrote a fresh copy, so its exec may see ETXTBSY.
        for _ in 0..100 {
            match std::process::Command::new(program)
                .arg("-c")
                .arg("read line")
                .env_remove("BASH_ENV")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                Ok(child) => return OwnedChild(child),
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("spawn waiting child: {error}"),
            }
        }
        panic!("spawn waiting child: executable stayed busy");
    }

    fn install_executable_copy(destination: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let staged = destination.with_extension("staged");
        fs::copy(system_shell(), &staged).expect("copy executable");
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
            .expect("make copy executable");
        fs::rename(&staged, destination).expect("install copy");
    }

    #[test]
    fn snapshots_a_process_running_an_installed_file() {
        let directory = TestDirectory::new();
        let executable = directory.0.join("client");
        install_executable_copy(&executable);
        let child = spawn_waiting(&executable);

        let identity = ExecutableIdentity::from_pid(child.0.id()).expect("snapshot child");
        let metadata = fs::metadata(&executable).expect("installed metadata");
        assert_eq!(identity.path, executable);
        assert_eq!(
            (identity.device, identity.inode),
            (metadata.dev(), metadata.ino())
        );
    }

    #[test]
    fn rejects_a_process_whose_executable_was_replaced_or_unlinked() {
        let directory = TestDirectory::new();
        let executable = directory.0.join("client");
        install_executable_copy(&executable);
        let child = spawn_waiting(&executable);

        // Replaced by rename: the path now names a different inode.
        install_executable_copy(&executable);
        assert!(ExecutableIdentity::from_pid(child.0.id()).is_err());

        // Unlinked: the path names nothing.
        fs::remove_file(&executable).expect("unlink executable");
        assert!(ExecutableIdentity::from_pid(child.0.id()).is_err());
    }

    #[test]
    fn rejects_a_process_running_a_memfd_image() {
        let memfd = rustix::fs::memfd_create("splinterm", rustix::fs::MemfdFlags::CLOEXEC)
            .expect("create memfd");
        let mut image = File::from(memfd);
        image
            .write_all(&fs::read(system_shell()).expect("read executable"))
            .expect("write memfd image");
        let child = spawn_waiting(Path::new(&format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&image)
        )));

        assert!(ExecutableIdentity::from_pid(child.0.id()).is_err());
    }

    #[test]
    fn binds_the_running_image_only_to_a_path_that_names_it() {
        let running = fs::canonicalize(std::env::current_exe().expect("test executable"))
            .expect("canonical test executable");
        let identity =
            ExecutableIdentity::of_current_process_at(&running).expect("snapshot running image");
        let reference = ExecutableIdentity::from_pid(std::process::id()).expect("snapshot self");
        assert_eq!(identity, reference);

        let directory = TestDirectory::new();
        let other = directory.0.join("other");
        fs::write(&other, b"other").expect("write other file");
        assert!(ExecutableIdentity::of_current_process_at(&other).is_err());
        assert!(ExecutableIdentity::of_current_process_at(Path::new("relative")).is_err());
        assert!(ExecutableIdentity::of_current_process_at(&directory.0.join("missing")).is_err());
    }
}
