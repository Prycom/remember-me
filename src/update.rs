//! HTTPS download, checksum/target validation and platform-aware executable replacement.
//! No background daemon, in-place writes, sudo, or restart during a call.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
fn binary_asset() -> &'static str {
    if cfg!(windows) {
        "remember-me.exe"
    } else {
        "remember-me"
    }
}
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW: no flashes while updating in a call.
    }
    #[cfg(not(windows))]
    let _ = &mut command;
    command
}
pub const BASE: &str = "https://github.com/Prycom/remember-me/releases/download/test";
pub fn build() -> u64 {
    env!("RM_BUILD_UNIX").parse().unwrap()
}
pub fn target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
pub fn build_info() -> String {
    format!(
        "version={}\nbuild={}\ntarget={}\n",
        env!("CARGO_PKG_VERSION"),
        build(),
        target()
    )
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub build: u64,
    pub target: String,
    pub sha256: String,
    pub size: u64,
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn checksum(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn download(url: &str, path: &Path, limit: u64) -> Result<(), String> {
    let status = command(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "5",
            "--max-time",
            "120",
            "--max-filesize",
            &limit.to_string(),
            "--output",
        ])
        .arg(path)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("нужен curl для обновления: {e}"))?;
    if !status.success() {
        return Err(format!(
            "не удалось скачать обновление (curl {status}); проверьте сеть и assets релиза test"
        ));
    }
    Ok(())
}
fn temporary(parent: &Path, label: &str) -> Result<Temporary, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = parent.join(format!(
        ".remember-me-{label}-{}-{nonce}{}",
        std::process::id(),
        if cfg!(windows) && label == "update" {
            ".tmp.exe"
        } else {
            ".tmp"
        }
    ));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("нет прав записи рядом с бинарником: {e}"))?;
    Ok(Temporary(path))
}
fn validate(path: &Path, manifest: &Manifest) -> Result<(), String> {
    if manifest.target != target() {
        return Err(format!(
            "релиз для {}, этот клиент для {}",
            manifest.target,
            target()
        ));
    }
    if manifest.size < 64
        || manifest.size > 64 * 1024 * 1024
        || fs::metadata(path).map_err(|e| e.to_string())?.len() != manifest.size
    {
        return Err("размер бинарника обновления не совпал".into());
    }
    if manifest.sha256.len() != 64
        || !manifest.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || checksum(path)? != manifest.sha256.to_ascii_lowercase()
    {
        return Err("SHA-256 обновления не совпал; текущий бинарник сохранён".into());
    }
    validate_executable(path, std::env::consts::OS, std::env::consts::ARCH)
}
fn validate_executable(path: &Path, os: &str, arch: &str) -> Result<(), String> {
    use std::io::{Seek, SeekFrom};
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut header = [0u8; 64];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    if os == "windows" {
        if &header[..2] != b"MZ" {
            return Err("скачан неподходящий Windows EXE".into());
        }
        let offset = u32::from_le_bytes(header[60..64].try_into().unwrap()) as u64;
        if !(64..=1024 * 1024).contains(&offset) {
            return Err("повреждён заголовок PE".into());
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut pe = [0; 26];
        file.read_exact(&mut pe)
            .map_err(|e| format!("заголовок PE: {e}"))?;
        let machine = match arch {
            "x86_64" => 0x8664,
            "aarch64" => 0xaa64,
            _ => return Err("неподдерживаемая архитектура EXE".into()),
        };
        if &pe[..4] != b"PE\0\0"
            || u16::from_le_bytes([pe[4], pe[5]]) != machine
            || u16::from_le_bytes([pe[24], pe[25]]) != 0x20b
        {
            return Err("скачан EXE другой архитектуры или повреждённый PE".into());
        }
    } else {
        let machine = match arch {
            "x86_64" => 62,
            "aarch64" => 183,
            _ => return Err("обновление этой архитектуры пока не поддерживается".into()),
        };
        if &header[..4] != b"\x7fELF"
            || header[4] != 2
            || header[5] != 1
            || u16::from_le_bytes([header[18], header[19]]) != machine
        {
            return Err("скачан неподходящий ELF-бинарник".into());
        }
    }
    Ok(())
}

fn install(current: &Path, replacement: &Path, manifest: &Manifest) -> Result<(), String> {
    validate(replacement, manifest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(current)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        fs::set_permissions(
            replacement,
            fs::Permissions::from_mode((mode & 0o777) | 0o100),
        )
        .map_err(|e| e.to_string())?;
    }
    fs::OpenOptions::new()
        .write(true)
        .open(replacement)
        .map_err(|e| e.to_string())?
        .sync_all()
        .map_err(|e| e.to_string())?;
    // A failed download or validation never touches the installed executable.
    // Unix uses atomic rename; Windows keeps a rollback copy while replacing
    // the locked EXE and schedules deletion of the old image after exit.
    #[cfg(windows)]
    if fs::canonicalize(current).ok() == std::env::current_exe().and_then(fs::canonicalize).ok() {
        // self-replace moves the locked running EXE aside and cleans it up
        // after exit. Calls and the terminal keep running until the user exits.
        // Retain a rollback copy: self-replace can fail after moving the locked
        // original aside (for example, if a virus scanner blocks the new file).
        let backup = temporary(current.parent().ok_or("нет папки EXE")?, "rollback")?;
        fs::copy(current, &backup.0).map_err(|e| format!("резервная копия EXE: {e}"))?;
        if let Err(error) = self_replace::self_replace(replacement) {
            if !current.exists()
                && let Err(restore) = fs::rename(&backup.0, current)
            {
                let path = backup.0.display().to_string();
                std::mem::forget(backup);
                return Err(format!(
                    "замена EXE: {error}; восстановление: {restore}; копия сохранена: {path}"
                ));
            }
            return Err(format!("замена EXE: {error}; исходная сборка сохранена"));
        }
    } else {
        fs::rename(replacement, current).map_err(|e| format!("замена EXE: {e}"))?;
    }
    #[cfg(not(windows))]
    fs::rename(replacement, current).map_err(|e| format!("замена бинарника: {e}"))?;
    if let Some(parent) = current.parent() {
        let _ = fs::File::open(parent).and_then(|f| f.sync_all());
    }
    Ok(())
}
fn probe(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut process = command(path)
        .arg("--build-info")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("новая сборка не запускается, текущая сохранена: {e}"))?;
    let started = std::time::Instant::now();
    loop {
        match process.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < std::time::Duration::from_secs(3) => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            _ => {
                let _ = process.kill();
                let _ = process.wait();
                return Err("проверка новой сборки не завершилась; текущая сохранена".into());
            }
        }
    }
    let output = process.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == format!("target={}", manifest.target))
    {
        return Err("новая сборка несовместима с системой/библиотеками; текущая сохранена".into());
    }
    Ok(())
}
pub fn run() -> Result<Option<String>, String> {
    // Serialize startup/F5 updates. On Windows self-replace must run only once
    // per process because the loader's current_exe path is moved aside.
    static UPDATED: std::sync::Mutex<bool> = std::sync::Mutex::new(false);
    let mut updated = UPDATED.lock().map_err(|e| e.to_string())?;
    if *updated {
        return Ok(Some(
            "бинарник уже обновлён; перезапустите клиент после звонка".into(),
        ));
    }
    static EXECUTABLE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let executable = if let Some(path) = EXECUTABLE.get() {
        path.clone()
    } else {
        let path = std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|e| e.to_string())?;
        let _ = EXECUTABLE.set(path.clone());
        path
    };
    let parent = executable.parent().ok_or("нет папки исполняемого файла")?;
    let metadata_file = temporary(&std::env::temp_dir(), "manifest")?;
    download(
        &format!("{BASE}/{}.manifest.toml", binary_asset()),
        &metadata_file.0,
        16384,
    )?;
    let manifest: Manifest =
        toml::from_str(&fs::read_to_string(&metadata_file.0).map_err(|e| e.to_string())?)
            .map_err(|e| format!("манифест обновления: {e}"))?;
    if manifest.build <= build() {
        return Ok(None);
    }
    if manifest.target != target() {
        return Err("в релизе нет сборки для этой платформы".into());
    }
    if checksum(&executable)? == manifest.sha256.to_ascii_lowercase() {
        return Ok(Some(
            "бинарник уже обновлён; перезапустите клиент после звонка".into(),
        ));
    }
    let binary = temporary(parent, "update")?;
    download(
        &format!("{BASE}/{}", binary_asset()),
        &binary.0,
        64 * 1024 * 1024,
    )?;
    validate(&binary.0, &manifest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&executable)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        fs::set_permissions(
            &binary.0,
            fs::Permissions::from_mode((mode & 0o777) | 0o100),
        )
        .map_err(|e| e.to_string())?;
    }
    probe(&binary.0, &manifest)?;
    install(&executable, &binary.0, &manifest)?;
    *updated = true;
    Ok(Some(format!(
        "обновлено до {} ({}); перезапустите клиент после звонка",
        manifest.version, manifest.build
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_architecture_and_corrupt_pe_offsets_before_replacement() {
        let path = std::env::temp_dir().join(format!("rm-pe-{}", std::process::id()));
        let mut bytes = vec![0; 128];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        assert!(validate_executable(&path, "windows", "x86_64").is_ok());
        assert!(validate_executable(&path, "windows", "aarch64").is_err());
        assert!(validate_executable(&path, "linux", "x86_64").is_err());
        bytes[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        assert!(validate_executable(&path, "windows", "x86_64").is_err());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn atomic_update_preserves_old_binary_on_bad_hash_and_replaces_on_success() {
        let dir = std::env::temp_dir().join(format!("rm-update-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let current = dir.join("remember-me");
        let incoming = dir.join("incoming");
        fs::write(&current, b"old running binary").unwrap();
        let mut bytes = vec![0; 128];
        if cfg!(windows) {
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
            bytes[64..68].copy_from_slice(b"PE\0\0");
            bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
            bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        } else {
            bytes[..4].copy_from_slice(b"\x7fELF");
            bytes[4] = 2;
            bytes[5] = 1;
            bytes[18] = if std::env::consts::ARCH == "aarch64" {
                183
            } else {
                62
            };
        }
        fs::write(&incoming, &bytes).unwrap();
        let mut manifest = Manifest {
            version: "test".into(),
            build: build() + 1,
            target: target(),
            sha256: "0".repeat(64),
            size: 128,
        };
        assert!(install(&current, &incoming, &manifest).is_err());
        assert_eq!(fs::read(&current).unwrap(), b"old running binary");
        manifest.sha256 = checksum(&incoming).unwrap();
        manifest.target = "wrong-platform".into();
        assert!(install(&current, &incoming, &manifest).is_err());
        manifest.target = target();
        install(&current, &incoming, &manifest).unwrap();
        assert_eq!(fs::read(&current).unwrap(), bytes);
        assert!(!incoming.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
