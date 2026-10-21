//! Persistent history lives in platform state storage, never beside settings.
use std::{
    io,
    path::{Path, PathBuf},
};

pub fn logs_directory() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::config::home_directory().join("AppData/Local"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::config::home_directory().join(".local/state"))
    };
    base.join("remember-me/logs")
}

pub fn private_directory(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

pub fn log_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

pub fn initialize() -> io::Result<()> {
    let config = crate::config::Config::path();
    let directory = config
        .parent()
        .ok_or_else(|| io::Error::other("нет папки настроек"))?;
    migrate(directory, &logs_directory())
}

fn move_file(source: &Path, target: &Path, replacements: Option<(&Path, &Path)>) -> io::Result<()> {
    use io::Write;
    // Publish a fully written copy atomically, without overwriting a destination.
    // Only then remove the original. Cross-filesystem moves work as well.
    let mut output = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    if let Some((old, new)) = replacements {
        let bytes = std::fs::read(source)?;
        match String::from_utf8(bytes) {
            Ok(mut body) => {
                // Paths inside TOML inside an escaped chat record may have one,
                // two or four backslashes. Replace the most escaped form first.
                for level in [4, 2, 1] {
                    let old = old.to_string_lossy().replace('\\', &"\\".repeat(level));
                    let new = new.to_string_lossy().replace('\\', &"\\".repeat(level));
                    body = body.replace(&old, &new);
                }
                output.write_all(body.as_bytes())?;
            }
            Err(error) => output.write_all(&error.into_bytes())?,
        }
    } else {
        io::copy(&mut std::fs::File::open(source)?, &mut output)?;
    }
    output.as_file().sync_all()?;
    output.persist_noclobber(target).map_err(|e| e.error)?;
    std::fs::remove_file(source)
}

fn migrate(config: &Path, logs: &Path) -> io::Result<()> {
    if !config.is_dir() {
        return Ok(());
    }
    private_directory(logs)?;
    // Keep old pasted attachments with their history. Newly pasted images are
    // temporary; these earlier persistent copies may be the user's only copy.
    let clipboard = config.join("clipboard");
    let archive = logs.parent().unwrap().join("attachments");
    if clipboard.is_dir() {
        private_directory(&archive)?;
        for entry in std::fs::read_dir(&clipboard)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let target = archive.join(entry.file_name());
                if !target.exists() {
                    move_file(&entry.path(), &target, None)?;
                }
            }
        }
        // Only remove an empty directory. Unknown files are never discarded.
        let _ = std::fs::remove_dir(&clipboard);
    }
    for entry in std::fs::read_dir(config)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || entry.path().extension().is_none_or(|e| e != "log") {
            continue;
        }
        let target = logs.join(entry.file_name());
        if target.exists() {
            // Preserve both versions when state storage already has this log.
            let alternate = logs.join(format!("legacy-{}", entry.file_name().to_string_lossy()));
            if !alternate.exists() {
                move_file(&entry.path(), &alternate, Some((&clipboard, &archive)))?;
            }
        } else {
            move_file(&entry.path(), &target, Some((&clipboard, &archive)))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_preserves_history_and_attachments_and_is_repeatable() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let logs = root.path().join("state/logs");
        private_directory(&config.join("clipboard")).unwrap();
        let old_image = config.join("clipboard/clipboard-old.png");
        std::fs::write(&old_image, b"existing attachment").unwrap();
        std::fs::write(config.join("config.toml"), "name = 'user'").unwrap();
        std::fs::write(
            config.join("chat-peer.log"),
            old_image.to_string_lossy().as_bytes(),
        )
        .unwrap();
        migrate(&config, &logs).unwrap();
        migrate(&config, &logs).unwrap();
        assert!(config.join("config.toml").exists());
        assert!(!config.join("chat-peer.log").exists());
        assert!(!config.join("clipboard").exists());
        assert_eq!(
            std::fs::read(root.path().join("state/attachments/clipboard-old.png")).unwrap(),
            b"existing attachment"
        );
        assert!(
            std::fs::read_to_string(logs.join("chat-peer.log"))
                .unwrap()
                .contains("attachments")
        );
        std::fs::write(config.join("chat-peer.log"), "another history").unwrap();
        migrate(&config, &logs).unwrap();
        assert_eq!(
            std::fs::read_to_string(logs.join("legacy-chat-peer.log")).unwrap(),
            "another history"
        );
        assert_ne!(
            std::fs::read_to_string(logs.join("chat-peer.log")).unwrap(),
            "another history"
        );
    }
}
