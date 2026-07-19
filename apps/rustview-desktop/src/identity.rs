//! Per-user persistence for the public RustView device identifier.

use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
};

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;

use anyhow::{Context as _, Result, anyhow, bail};
use rustview_core::DeviceId;

const DEVICE_ID_FILE: &str = "device-id";
const CONFIG_DIR_OVERRIDE: &str = "RUSTVIEW_CONFIG_DIR";
const DEVICE_ID_LOCK_FILE: &str = ".device-id.lock";
const SIDECAR_ATTEMPTS: usize = 16;
const RANDOM_SUFFIX_BYTES: usize = 12;
const MAX_DEVICE_ID_FILE_BYTES: u64 = 64;
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(test)]
const TEST_CHILD_PATH_ENV: &str = "RUSTVIEW_IDENTITY_TEST_CHILD_PATH";
#[cfg(test)]
const TEST_BEFORE_LOCK_BARRIER_ENV: &str = "RUSTVIEW_IDENTITY_TEST_BEFORE_LOCK_BARRIER";
#[cfg(test)]
const TEST_BARRIER_PARTIES_ENV: &str = "RUSTVIEW_IDENTITY_TEST_BARRIER_PARTIES";

/// Loads this installation's stable ID, creating it on the first launch.
///
/// Only the non-secret device ID is persisted. Access passwords remain
/// intentionally per-process and are never handled by this module.
pub fn load_or_create_device_id() -> Result<DeviceId> {
    load_or_create_device_id_at(&device_id_path()?)
}

/// Resolves RustView's per-user configuration directory.
///
/// The directory contains only local application configuration. Access
/// passwords and derived Noise secrets are never persisted here.
pub(crate) fn config_directory() -> Result<PathBuf> {
    let directory = platform_config_directory()?;
    absolute_path(&directory)
}

/// Creates a configuration directory with durable parent metadata updates.
pub(crate) fn ensure_config_directory(path: &Path) -> Result<()> {
    create_directory_tree_durable(path)
}

/// Flushes directory metadata after publishing a configuration file.
pub(crate) fn sync_config_directory(path: &Path) -> Result<()> {
    sync_parent_directory(path)
        .with_context(|| format!("failed to sync config directory {}", path.display()))
}

fn load_or_create_device_id_at(path: &Path) -> Result<DeviceId> {
    let absolute_path = absolute_path(path)?;
    let path = absolute_path.as_path();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("device ID path has no parent: {}", path.display()))?;
    create_directory_tree_durable(parent)?;

    // Existing healthy installations avoid the lock, but still establish a
    // directory metadata durability barrier before reporting success. Any
    // create or recovery operation acquires the cross-process OS lock and then
    // re-inspects state, closing the legacy recovery TOCTOU window.
    if let Ok(DeviceIdFile::Valid(id)) = inspect_device_id(path) {
        sync_parent_directory(parent).with_context(|| {
            format!(
                "failed to sync existing device ID metadata in {}",
                parent.display()
            )
        })?;
        return Ok(id);
    }

    #[cfg(test)]
    wait_at_before_lock_test_barrier()?;

    let _device_id_lock = DeviceIdLock::acquire(parent)?;

    loop {
        match inspect_device_id(path)? {
            DeviceIdFile::Valid(id) => return Ok(id),
            DeviceIdFile::Missing => return create_and_publish_device_id(path, parent),
            DeviceIdFile::RecoverablePartial => {
                // RustView's original persistence path created the final file
                // before writing it. Preserve evidence from an interrupted old
                // write, then retry through the crash-safe publication path.
                quarantine_partial_device_id(path, parent)?;
            }
        }
    }
}

fn create_and_publish_device_id(path: &Path, parent: &Path) -> Result<DeviceId> {
    let generated = DeviceId::generate().context("failed to generate device ID")?;
    let temporary = write_synced_temporary_device_id(path, generated)?;

    match fs::hard_link(temporary.path(), path) {
        Ok(()) => {
            // The link is no-replace and points at an already complete, synced
            // inode. At no time can a reader observe a partial final file.
            sync_parent_directory(parent).with_context(|| {
                format!("failed to sync published device ID in {}", parent.display())
            })?;
            if let Err(error) = temporary.remove() {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "published device ID but could not remove temporary file"
                );
            } else {
                sync_parent_directory(parent).with_context(|| {
                    format!("failed to sync device ID cleanup in {}", parent.display())
                })?;
            }
            Ok(generated)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            // This can only be an external/non-cooperating publisher because
            // RustView creators hold the device lock. Establish the metadata
            // durability barrier before treating its final file as successful.
            sync_parent_directory(parent).with_context(|| {
                format!("failed to sync winning device ID in {}", parent.display())
            })?;
            read_device_id(path).context("failed to read concurrently published device ID")
        }
        Err(error) => Err(error).context("failed to atomically publish device ID"),
    }
}

fn read_device_id(path: &Path) -> io::Result<DeviceId> {
    let contents = read_device_id_bytes(path)?;
    parse_device_id_bytes(path, &contents)
}

fn read_device_id_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut contents = Vec::with_capacity(MAX_DEVICE_ID_FILE_BYTES as usize);
    (&mut file)
        .take(MAX_DEVICE_ID_FILE_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_DEVICE_ID_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "device ID in {} exceeds {} bytes",
                path.display(),
                MAX_DEVICE_ID_FILE_BYTES
            ),
        ));
    }
    Ok(contents)
}

fn parse_device_id_bytes(path: &Path, contents: &[u8]) -> io::Result<DeviceId> {
    let contents = std::str::from_utf8(contents).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("device ID in {} is not UTF-8: {error}", path.display()),
        )
    })?;
    contents.trim().parse::<DeviceId>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid device ID in {}: {error}", path.display()),
        )
    })
}

#[derive(Debug)]
enum DeviceIdFile {
    Missing,
    Valid(DeviceId),
    RecoverablePartial,
}

fn inspect_device_id(path: &Path) -> Result<DeviceIdFile> {
    let contents = match read_device_id_bytes(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DeviceIdFile::Missing);
        }
        Err(error) => return Err(error).context("failed to read persisted device ID"),
    };

    match parse_device_id_bytes(path, &contents) {
        Ok(id) => Ok(DeviceIdFile::Valid(id)),
        Err(_) if is_legacy_partial_write(&contents) => Ok(DeviceIdFile::RecoverablePartial),
        Err(error) => Err(error).context("persisted device ID is corrupt and was not replaced"),
    }
}

fn is_legacy_partial_write(contents: &[u8]) -> bool {
    let without_line_ending = contents
        .strip_suffix(b"\n")
        .unwrap_or(contents)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| contents.strip_suffix(b"\n").unwrap_or(contents));

    without_line_ending.len() < 9 && without_line_ending.iter().all(|byte| byte.is_ascii_digit())
}

fn write_synced_temporary_device_id(
    final_path: &Path,
    device_id: DeviceId,
) -> Result<TemporaryPath> {
    let parent = final_path
        .parent()
        .ok_or_else(|| anyhow!("device ID path has no parent: {}", final_path.display()))?;
    let file_name = final_path
        .file_name()
        .ok_or_else(|| anyhow!("device ID path has no filename: {}", final_path.display()))?
        .to_string_lossy();

    for _ in 0..SIDECAR_ATTEMPTS {
        let temporary_path = parent.join(format!(".{file_name}.tmp-{}", random_filename_suffix()?));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(mut file) => {
                let temporary = TemporaryPath::new(temporary_path);
                writeln!(file, "{}", device_id.canonical_digits())
                    .context("failed to write temporary device ID")?;
                file.sync_all()
                    .context("failed to sync temporary device ID")?;
                drop(file);
                return Ok(temporary);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("failed to create temporary device ID"),
        }
    }
    bail!("could not allocate a unique temporary device ID file")
}

fn quarantine_partial_device_id(path: &Path, parent: &Path) -> Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("device ID path has no filename: {}", path.display()))?
        .to_string_lossy();

    for _ in 0..SIDECAR_ATTEMPTS {
        let backup = parent.join(format!("{file_name}.corrupt-{}", random_filename_suffix()?));
        match fs::rename(path, &backup) {
            Ok(()) => {
                sync_parent_directory(parent).with_context(|| {
                    format!("failed to sync corrupt ID backup in {}", parent.display())
                })?;
                tracing::warn!(
                    source = %path.display(),
                    backup = %backup.display(),
                    "recovered an incomplete device ID from an earlier interrupted write"
                );
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("failed to preserve incomplete device ID"),
        }
    }
    bail!("could not allocate a unique corrupt device ID backup")
}

fn random_filename_suffix() -> Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0_u8; RANDOM_SUFFIX_BYTES];
    getrandom::fill(&mut bytes).context("failed to generate temporary filename")?;
    let mut suffix = String::with_capacity(RANDOM_SUFFIX_BYTES * 2);
    for byte in bytes {
        suffix.push(char::from(HEX[usize::from(byte >> 4)]));
        suffix.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(suffix)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(env::current_dir()
        .context("failed to resolve current directory for device ID")?
        .join(path))
}

fn create_directory_tree_durable(path: &Path) -> Result<()> {
    let absolute = absolute_path(path)?;
    let mut missing = Vec::new();
    let mut cursor = absolute.as_path();

    loop {
        match fs::metadata(cursor) {
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => bail!(
                "device config path component is not a directory: {}",
                cursor.display()
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(cursor.to_path_buf());
                cursor = cursor.parent().ok_or_else(|| {
                    anyhow!(
                        "device config path has no existing ancestor: {}",
                        absolute.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect config directory {}", cursor.display())
                });
            }
        }
    }

    if missing.is_empty() {
        if let Some(parent) = absolute.parent() {
            sync_parent_directory(parent).with_context(|| {
                format!(
                    "failed to sync existing config directory entry {}",
                    absolute.display()
                )
            })?;
        }
        return Ok(());
    }

    for directory in missing.iter().rev() {
        let parent = directory.parent().ok_or_else(|| {
            anyhow!(
                "config directory has no parent to synchronize: {}",
                directory.display()
            )
        })?;
        match fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !directory.is_dir() {
                    return Err(error).with_context(|| {
                        format!(
                            "config path was concurrently created as a file: {}",
                            directory.display()
                        )
                    });
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to create config directory {}", directory.display())
                });
            }
        }
        sync_parent_directory(parent).with_context(|| {
            format!(
                "failed to sync new config directory entry {}",
                directory.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    // CreateFile requires FILE_FLAG_BACKUP_SEMANTICS for directory handles;
    // write access lets File::sync_all call FlushFileBuffers for metadata.
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(parent)?
        .sync_all()
}

#[cfg(not(any(unix, windows)))]
fn sync_parent_directory(_parent: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable directory synchronization is unsupported on this platform",
    ))
}

#[derive(Debug)]
struct DeviceIdLock {
    _file: File,
}

impl DeviceIdLock {
    fn acquire(parent: &Path) -> Result<Self> {
        let lock_path = parent.join(DEVICE_ID_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("failed to open device ID lock {}", lock_path.display()))?;
        File::lock(&file)
            .with_context(|| format!("failed to lock device ID state {}", lock_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync device ID lock {}", lock_path.display()))?;
        sync_parent_directory(parent).with_context(|| {
            format!(
                "failed to sync device ID lock entry in {}",
                parent.display()
            )
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
fn wait_at_before_lock_test_barrier() -> Result<()> {
    use std::{thread, time::Duration};

    let Some(barrier_path) = env::var_os(TEST_BEFORE_LOCK_BARRIER_ENV) else {
        return Ok(());
    };
    let parties = env::var(TEST_BARRIER_PARTIES_ENV)
        .context("test barrier party count is missing")?
        .parse::<usize>()
        .context("test barrier party count is invalid")?;
    let barrier_path = PathBuf::from(barrier_path);
    let ready = barrier_path.join(format!("ready-{}", std::process::id()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ready)
        .with_context(|| format!("failed to enter test barrier {}", ready.display()))?
        .sync_all()
        .context("failed to sync test barrier marker")?;

    for _ in 0..2_000 {
        let ready_count = fs::read_dir(&barrier_path)
            .with_context(|| format!("failed to read test barrier {}", barrier_path.display()))?
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("ready-"))
            .count();
        if ready_count >= parties {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(5));
    }
    bail!("timed out waiting for device ID test processes before lock")
}

#[derive(Debug)]
struct TemporaryPath {
    path: Option<PathBuf>,
}

impl TemporaryPath {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("temporary path is available")
    }

    fn remove(mut self) -> io::Result<()> {
        let path = self.path.take().expect("temporary path is available");
        fs::remove_file(path)
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn device_id_path() -> Result<PathBuf> {
    Ok(config_directory()?.join(DEVICE_ID_FILE))
}

#[cfg(target_os = "macos")]
fn platform_config_directory() -> Result<PathBuf> {
    if let Some(path) = config_override_directory(env::var_os(CONFIG_DIR_OVERRIDE))? {
        return Ok(path);
    }
    let home = required_env_path("HOME")?;
    Ok(home
        .join("Library")
        .join("Application Support")
        .join("RustView"))
}

#[cfg(target_os = "windows")]
fn platform_config_directory() -> Result<PathBuf> {
    if let Some(path) = config_override_directory(env::var_os(CONFIG_DIR_OVERRIDE))? {
        return Ok(path);
    }
    Ok(required_env_path("APPDATA")?.join("RustView"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_config_directory() -> Result<PathBuf> {
    if let Some(path) = config_override_directory(env::var_os(CONFIG_DIR_OVERRIDE))? {
        return Ok(path);
    }
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(config_home).join("rustview"));
    }
    Ok(required_env_path("HOME")?.join(".config").join("rustview"))
}

fn config_override_directory(value: Option<OsString>) -> Result<Option<PathBuf>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        bail!("{CONFIG_DIR_OVERRIDE} is empty");
    }
    Ok(Some(PathBuf::from(value)))
}

fn required_env_path(name: &str) -> Result<PathBuf> {
    let value = env::var_os(name).ok_or_else(|| anyhow!("{name} is not set"))?;
    if value.is_empty() {
        bail!("{name} is empty");
    }
    Ok(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Output, Stdio},
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;

    struct TempConfig {
        root: PathBuf,
    }

    impl TempConfig {
        fn new() -> Self {
            let suffix = DeviceId::generate()
                .expect("OS randomness")
                .canonical_digits();
            let root = env::temp_dir().join(format!("rustview-identity-test-{suffix}"));
            Self { root }
        }

        fn id_path(&self) -> PathBuf {
            self.root.join("nested").join(DEVICE_ID_FILE)
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn creates_parent_and_reuses_persisted_id() {
        let temp = TempConfig::new();
        let path = temp.id_path();
        let created = load_or_create_device_id_at(&path).expect("create device ID");
        let loaded = load_or_create_device_id_at(&path).expect("load device ID");
        assert_eq!(created, loaded);
        assert_eq!(
            fs::read_to_string(path).expect("read persisted ID"),
            format!("{}\n", created.canonical_digits())
        );
        assert!(sidecars(&temp.root, ".tmp-").is_empty());
    }

    #[test]
    fn final_path_is_absent_until_synced_temporary_file_is_complete() {
        let temp = TempConfig::new();
        let path = temp.id_path();
        let parent = path.parent().expect("parent");
        fs::create_dir_all(parent).expect("create parent");
        let id: DeviceId = "123 456 789".parse().expect("valid ID");

        let temporary = write_synced_temporary_device_id(&path, id).expect("write temporary ID");
        assert!(!path.exists(), "final path must not exist before publish");
        assert_eq!(
            fs::read_to_string(temporary.path()).expect("read temporary ID"),
            "123456789\n"
        );

        fs::hard_link(temporary.path(), &path).expect("publish complete ID");
        assert_eq!(
            fs::read_to_string(&path).expect("read final ID"),
            "123456789\n"
        );
        temporary.remove().expect("remove temporary ID");
    }

    #[test]
    fn config_override_places_id_directly_in_requested_directory() {
        let temp = TempConfig::new();
        let overridden_directory =
            config_override_directory(Some(temp.root.clone().into_os_string()))
                .expect("valid override")
                .expect("override directory");
        let overridden = overridden_directory.join(DEVICE_ID_FILE);
        assert_eq!(overridden, temp.root.join(DEVICE_ID_FILE));
        assert_eq!(
            overridden_directory.join("relay-address"),
            temp.root.join("relay-address")
        );

        let created = load_or_create_device_id_at(&overridden).expect("create overridden ID");
        assert_eq!(
            fs::read_to_string(overridden).expect("read overridden ID"),
            format!("{}\n", created.canonical_digits())
        );
    }

    #[test]
    fn concurrent_first_launches_converge_on_one_id() {
        const WORKERS: usize = 8;
        let temp = TempConfig::new();
        let path = Arc::new(temp.id_path());
        let barrier = Arc::new(Barrier::new(WORKERS));
        let handles: Vec<_> = (0..WORKERS)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    load_or_create_device_id_at(&path)
                })
            })
            .collect();

        let ids: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("worker did not panic").expect("ID"))
            .collect();
        assert!(ids.iter().all(|id| *id == ids[0]));
        assert_eq!(
            fs::read_to_string(path.as_ref()).expect("read winning ID"),
            format!("{}\n", ids[0].canonical_digits())
        );
        assert!(sidecars(&temp.root, ".tmp-").is_empty());
    }

    #[test]
    fn incomplete_legacy_files_are_backed_up_then_recovered() {
        for (case, incomplete) in [("empty", ""), ("partial", "1234567\n")] {
            let temp = TempConfig::new();
            let path = temp.root.join(case).join(DEVICE_ID_FILE);
            fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
            fs::write(&path, incomplete).expect("write incomplete fixture");

            let recovered = load_or_create_device_id_at(&path).expect("recover device ID");
            assert_eq!(
                fs::read_to_string(&path).expect("read recovered ID"),
                format!("{}\n", recovered.canonical_digits())
            );

            let backups = sidecars(path.parent().expect("parent"), ".corrupt-");
            assert_eq!(backups.len(), 1);
            assert_eq!(
                fs::read_to_string(&backups[0]).expect("read corrupt backup"),
                incomplete
            );
        }
    }

    #[test]
    fn two_processes_recover_one_legacy_partial_without_moving_new_final() {
        let temp = TempConfig::new();
        let path = temp.id_path();
        let barrier = temp.root.join("process-barrier");
        fs::create_dir_all(path.parent().expect("parent")).expect("create ID parent");
        fs::create_dir_all(&barrier).expect("create process barrier");
        fs::write(&path, "12345").expect("write partial legacy ID");

        let first = identity_child_command(&path, &barrier)
            .spawn()
            .expect("spawn first identity process");
        let second = identity_child_command(&path, &barrier)
            .spawn()
            .expect("spawn second identity process");
        let first_output = first.wait_with_output().expect("wait for first process");
        let second_output = second.wait_with_output().expect("wait for second process");

        let first_id = child_id(&first_output);
        let second_id = child_id(&second_output);
        assert_eq!(first_id, second_id);
        assert_eq!(
            fs::read_to_string(&path).expect("read recovered final ID"),
            format!("{first_id}\n")
        );
        let backups = sidecars(path.parent().expect("parent"), ".corrupt-");
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read_to_string(&backups[0]).expect("read preserved partial ID"),
            "12345"
        );
    }

    #[test]
    fn persistence_subprocess() {
        let Some(path) = env::var_os(TEST_CHILD_PATH_ENV) else {
            return;
        };
        let id = load_or_create_device_id_at(Path::new(&path)).expect("child device ID");
        println!("RUSTVIEW_CHILD_ID={}", id.canonical_digits());
    }

    #[test]
    fn invalid_existing_id_is_not_silently_replaced() {
        let temp = TempConfig::new();
        let path = temp.id_path();
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(&path, "not-an-id\n").expect("write invalid fixture");

        assert!(load_or_create_device_id_at(&path).is_err());
        assert_eq!(
            fs::read_to_string(path).expect("fixture remains"),
            "not-an-id\n"
        );
        assert!(sidecars(&temp.root, ".corrupt-").is_empty());
    }

    #[test]
    fn oversized_existing_id_is_rejected_without_unbounded_read() {
        let temp = TempConfig::new();
        let path = temp.id_path();
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        let oversized = vec![b'7'; MAX_DEVICE_ID_FILE_BYTES as usize + 1];
        fs::write(&path, &oversized).expect("write oversized fixture");

        assert!(load_or_create_device_id_at(&path).is_err());
        assert_eq!(fs::read(path).expect("fixture remains"), oversized);
    }

    fn identity_child_command(path: &Path, barrier: &Path) -> Command {
        let mut command = Command::new(env::current_exe().expect("current test executable"));
        command
            .args([
                "--exact",
                "identity::tests::persistence_subprocess",
                "--nocapture",
            ])
            .env(TEST_CHILD_PATH_ENV, path)
            .env(TEST_BEFORE_LOCK_BARRIER_ENV, barrier)
            .env(TEST_BARRIER_PARTIES_ENV, "2")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn child_id(output: &Output) -> String {
        assert!(
            output.status.success(),
            "identity child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("RUSTVIEW_CHILD_ID="))
            .expect("child ID marker")
            .to_owned()
    }

    fn sidecars(root: &Path, marker: &str) -> Vec<PathBuf> {
        let mut matches = Vec::new();
        collect_sidecars(root, marker, &mut matches);
        matches
    }

    fn collect_sidecars(root: &Path, marker: &str, matches: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_sidecars(&path, marker, matches);
            } else if entry.file_name().to_string_lossy().contains(marker) {
                matches.push(path);
            }
        }
    }
}
