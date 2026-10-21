//! HTTPS download, checksum/target validation and atomic executable replacement.
//! No background daemon, in-place writes, sudo, or restart during a call.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
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
    let status = Command::new("curl")
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
        ".remember-me-{label}-{}-{nonce}.tmp",
        std::process::id()
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
    let mut header = [0; 64];
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .read_exact(&mut header)
        .map_err(|e| e.to_string())?;
    let machine = match std::env::consts::ARCH {
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
    // The temporary file is on the same filesystem. A failed download or
    // validation never touches the installed executable; rename is atomic.
    fs::rename(replacement, current).map_err(|e| format!("замена бинарника: {e}"))?;
    if let Some(parent) = current.parent() {
        let _ = fs::File::open(parent).and_then(|f| f.sync_all());
    }
    Ok(())
}
fn probe(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut process = Command::new(path)
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
        &format!("{BASE}/remember-me.manifest.toml"),
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
    download(&format!("{BASE}/remember-me"), &binary.0, 64 * 1024 * 1024)?;
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
    Ok(Some(format!(
        "обновлено до {} ({}); перезапустите клиент после звонка",
        manifest.version, manifest.build
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_update_preserves_old_binary_on_bad_hash_and_replaces_on_success() {
        let dir = std::env::temp_dir().join(format!("rm-update-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let current = dir.join("remember-me");
        let incoming = dir.join("incoming");
        fs::write(&current, b"old running binary").unwrap();
        let mut bytes = vec![0; 128];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[18] = if std::env::consts::ARCH == "aarch64" {
            183
        } else {
            62
        };
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
