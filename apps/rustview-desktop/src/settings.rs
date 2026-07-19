//! Small, non-secret desktop settings persisted in RustView's config directory.

use std::{
    env,
    fs::{File, OpenOptions},
    io::{self, Read, Seek as _, SeekFrom, Write as _},
    path::Path,
};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::identity::{config_directory, ensure_config_directory, sync_config_directory};

pub(crate) const DEFAULT_RELAY_ADDRESS: &str = "127.0.0.1:21116";
pub(crate) const MAX_RELAY_ADDRESS_LEN: usize = 255;

const RELAY_ENV: &str = "RUSTVIEW_RELAY";
const RELAY_ADDRESS_FILE: &str = "relay-address";
const RELAY_ADDRESS_LOCK_FILE: &str = ".relay-address.lock";
const RECORD_PREFIX: &str = "RVRELAY1\t";
const MAX_SETTINGS_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub(crate) struct InitialRelayAddress {
    pub address: String,
    pub warning: Option<String>,
}

/// Resolves the startup relay address with environment > saved setting > default
/// precedence. Invalid external state never prevents the desktop UI from opening.
pub(crate) fn initial_relay_address() -> InitialRelayAddress {
    match env::var(RELAY_ENV) {
        Ok(value) => match normalize_relay_address(&value) {
            Ok(address) => InitialRelayAddress {
                address,
                warning: None,
            },
            Err(_) => choose_initial_relay(Some(&value), load_saved_relay_address()),
        },
        Err(env::VarError::NotPresent) => choose_initial_relay(None, load_saved_relay_address()),
        Err(env::VarError::NotUnicode(_)) => {
            let mut initial = choose_initial_relay(None, load_saved_relay_address());
            prepend_warning(
                &mut initial.warning,
                "RUSTVIEW_RELAY geçerli bir metin olmadığı için kullanılmadı.",
            );
            initial
        }
    }
}

/// Persists a validated relay address and returns its normalized form.
pub(crate) fn save_relay_address(value: &str) -> Result<String> {
    let address = normalize_relay_address(value)?;
    let directory = config_directory()?;
    ensure_config_directory(&directory)?;
    save_relay_address_at(&directory.join(RELAY_ADDRESS_FILE), &address)?;
    Ok(address)
}

fn choose_initial_relay(
    environment: Option<&str>,
    saved: Result<Option<String>>,
) -> InitialRelayAddress {
    let mut warnings = Vec::new();
    if let Some(value) = environment {
        match normalize_relay_address(value) {
            Ok(address) => {
                return InitialRelayAddress {
                    address,
                    warning: None,
                };
            }
            Err(error) => warnings.push(format!("RUSTVIEW_RELAY kullanılmadı: {error}")),
        }
    }

    let address = match saved {
        Ok(Some(address)) => address,
        Ok(None) => DEFAULT_RELAY_ADDRESS.to_owned(),
        Err(error) => {
            warnings.push(format!("Kayıtlı relay ayarı okunamadı: {error:#}"));
            DEFAULT_RELAY_ADDRESS.to_owned()
        }
    };
    InitialRelayAddress {
        address,
        warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
    }
}

fn prepend_warning(warning: &mut Option<String>, prefix: &str) {
    *warning = Some(match warning.take() {
        Some(existing) => format!("{prefix} {existing}"),
        None => prefix.to_owned(),
    });
}

fn load_saved_relay_address() -> Result<Option<String>> {
    let directory = config_directory()?;
    load_relay_address_at(&directory.join(RELAY_ADDRESS_FILE))
}

fn load_relay_address_at(path: &Path) -> Result<Option<String>> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("relay setting path has no parent: {}", path.display()))?;
    if !parent.exists() {
        return Ok(None);
    }

    let _lock = SettingsLock::acquire(parent)?;
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to open saved relay address"),
    };
    let size = file
        .metadata()
        .context("failed to inspect saved relay address")?
        .len();
    if size > MAX_SETTINGS_FILE_BYTES {
        bail!("saved relay setting exceeds its size limit");
    }

    let contents = read_bounded(&mut file, MAX_SETTINGS_FILE_BYTES)
        .context("failed to read saved relay address")?;
    if contents.is_empty() {
        bail!("saved relay setting is empty");
    }

    let mut latest = None;
    for record in contents.split_inclusive(|byte| *byte == b'\n') {
        if !record.ends_with(b"\n") {
            // A process may have stopped before syncing its final append. The
            // preceding complete record remains the committed setting.
            continue;
        }
        if let Some(address) = parse_record(record) {
            latest = Some(address);
        }
    }
    latest
        .map(Some)
        .ok_or_else(|| anyhow!("saved relay setting contains no complete valid record"))
}

fn save_relay_address_at(path: &Path, address: &str) -> Result<()> {
    let address = normalize_relay_address(address)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("relay setting path has no parent: {}", path.display()))?;
    ensure_config_directory(parent)?;
    let _lock = SettingsLock::acquire(parent)?;

    let record = format!("{RECORD_PREFIX}{}\t{address}\n", address.len());
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("failed to open relay setting {}", path.display()))?;
    let advertised_size = file
        .metadata()
        .context("failed to inspect relay settings journal")?
        .len();
    if advertised_size > MAX_SETTINGS_FILE_BYTES {
        bail!("relay settings journal reached its size limit");
    }
    let existing = read_bounded(&mut file, MAX_SETTINGS_FILE_BYTES)
        .context("failed to inspect relay settings journal tail")?;
    let committed_size = existing
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if committed_size < existing.len() {
        // Remove an interrupted append before adding the next committed
        // record; otherwise both byte sequences would become one invalid line.
        file.set_len(committed_size as u64)
            .context("failed to discard incomplete relay setting")?;
    }
    let record_size = u64::try_from(record.len()).context("relay setting record is too large")?;
    if (committed_size as u64).saturating_add(record_size) > MAX_SETTINGS_FILE_BYTES {
        bail!("relay settings journal reached its size limit");
    }
    file.seek(SeekFrom::Start(committed_size as u64))
        .context("failed to seek relay settings journal")?;
    file.write_all(record.as_bytes())
        .context("failed to append relay setting")?;
    file.sync_all().context("failed to sync relay setting")?;
    sync_config_directory(parent)?;
    Ok(())
}

fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut contents = Vec::with_capacity(usize::try_from(limit.min(4096)).unwrap_or(0));
    reader
        .take(limit + 1)
        .read_to_end(&mut contents)
        .context("failed to read bounded settings data")?;
    if contents.len() as u64 > limit {
        bail!("settings data exceeds its size limit");
    }
    Ok(contents)
}

fn parse_record(record: &[u8]) -> Option<String> {
    let record = record.strip_suffix(b"\n")?;
    let record = record.strip_suffix(b"\r").unwrap_or(record);
    let record = std::str::from_utf8(record).ok()?;
    let payload = record.strip_prefix(RECORD_PREFIX)?;
    let (encoded_length, address) = payload.split_once('\t')?;
    let encoded_length = encoded_length.parse::<usize>().ok()?;
    if encoded_length != address.len() {
        return None;
    }
    normalize_relay_address(address).ok()
}

fn normalize_relay_address(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("relay adresi boş bırakılamaz");
    }
    if value.len() > MAX_RELAY_ADDRESS_LEN {
        bail!("relay adresi en fazla {MAX_RELAY_ADDRESS_LEN} karakter olabilir");
    }
    if !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        bail!("relay adresi boşluk veya geçersiz karakter içeremez");
    }
    let (host, port) = value
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("relay adresi host:port biçiminde olmalıdır"))?;
    if host.is_empty() || host.contains('/') {
        bail!("relay host değeri geçersiz");
    }
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        bail!("IPv6 relay adresi [adres]:port biçiminde olmalıdır");
    }
    if matches!(host, "[]" | "[::]") {
        bail!("relay host değeri bağlanılabilir bir adres olmalıdır");
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| anyhow!("relay port değeri 1-65535 aralığında olmalıdır"))?;
    if port == 0 {
        bail!("relay port değeri 1-65535 aralığında olmalıdır");
    }
    Ok(value.to_owned())
}

#[derive(Debug)]
struct SettingsLock {
    _file: File,
}

impl SettingsLock {
    fn acquire(parent: &Path) -> Result<Self> {
        let path = parent.join(RELAY_ADDRESS_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("failed to open relay setting lock {}", path.display()))?;
        File::lock(&file)
            .with_context(|| format!("failed to lock relay settings {}", path.display()))?;
        file.sync_all()
            .context("failed to sync relay setting lock")?;
        sync_config_directory(parent)?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    struct TempSettings {
        root: PathBuf,
    }

    impl TempSettings {
        fn new() -> Self {
            let mut random = [0_u8; 12];
            getrandom::fill(&mut random).expect("OS randomness");
            let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            Self {
                root: env::temp_dir().join(format!("rustview-settings-test-{suffix}")),
            }
        }

        fn path(&self) -> PathBuf {
            self.root.join(RELAY_ADDRESS_FILE)
        }
    }

    impl Drop for TempSettings {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn saved_address_round_trips_and_latest_record_wins() {
        let temp = TempSettings::new();
        save_relay_address_at(&temp.path(), "relay-one.example:21116").expect("save first");
        save_relay_address_at(&temp.path(), "relay-two.example:443").expect("save second");

        assert_eq!(
            load_relay_address_at(&temp.path()).expect("load relay"),
            Some("relay-two.example:443".to_owned())
        );
    }

    #[test]
    fn incomplete_append_does_not_replace_last_committed_setting() {
        let temp = TempSettings::new();
        save_relay_address_at(&temp.path(), "relay.example:21116").expect("save relay");
        let mut file = OpenOptions::new()
            .append(true)
            .open(temp.path())
            .expect("open journal");
        file.write_all(b"RVRELAY1\t24\tpartial.example:4")
            .expect("append partial record");
        file.sync_all().expect("sync fixture");

        assert_eq!(
            load_relay_address_at(&temp.path()).expect("load prior relay"),
            Some("relay.example:21116".to_owned())
        );

        save_relay_address_at(&temp.path(), "recovered.example:443")
            .expect("save after partial append");
        assert_eq!(
            load_relay_address_at(&temp.path()).expect("load recovered relay"),
            Some("recovered.example:443".to_owned())
        );
    }

    #[test]
    fn invalid_or_empty_record_is_rejected() {
        let temp = TempSettings::new();
        ensure_config_directory(&temp.root).expect("create settings directory");
        fs::write(temp.path(), "not-a-settings-record\n").expect("write invalid fixture");
        assert!(load_relay_address_at(&temp.path()).is_err());

        fs::write(temp.path(), "").expect("write empty fixture");
        assert!(load_relay_address_at(&temp.path()).is_err());
    }

    #[test]
    fn environment_precedes_saved_address_and_invalid_env_falls_back() {
        let from_environment = choose_initial_relay(
            Some("env-relay.example:443"),
            Ok(Some("saved-relay.example:21116".to_owned())),
        );
        assert_eq!(from_environment.address, "env-relay.example:443");
        assert!(from_environment.warning.is_none());

        let from_saved = choose_initial_relay(
            Some("https://invalid.example"),
            Ok(Some("saved-relay.example:21116".to_owned())),
        );
        assert_eq!(from_saved.address, "saved-relay.example:21116");
        assert!(from_saved.warning.is_some());
    }

    #[test]
    fn validation_accepts_hostnames_and_bracketed_ipv6() {
        assert_eq!(
            normalize_relay_address(" relay.example:21116 ").expect("hostname"),
            "relay.example:21116"
        );
        assert!(normalize_relay_address("[::1]:21116").is_ok());
        for invalid in ["", "relay", "relay:0", "http://relay:80", "::1:21116"] {
            assert!(
                normalize_relay_address(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn bounded_reader_stops_an_unending_source() {
        let error = read_bounded(io::repeat(0), 32).expect_err("repeat source must exceed limit");
        assert!(error.to_string().contains("size limit"));
    }
}
