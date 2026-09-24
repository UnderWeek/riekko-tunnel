//! On-disk persistence of the user's profiles, groups and settings.
//! Everything else in `AppState` is runtime status and starts fresh.

use crate::state::{AppState, PersistedState};
use std::io::Write as _;
use std::path::Path;

/// Why the saved library couldn't be used.
pub enum LoadError {
    /// The file exists but couldn't be read (permissions, I/O): it must not
    /// be overwritten, since it may be perfectly fine.
    Unreadable(String),
    /// The file doesn't parse; it was moved aside so a fresh one can start.
    Corrupt(String),
}

/// `Ok(None)` on a first launch. A broken file is never silently replaced
/// by an empty library: it's either left alone (unreadable) or moved aside
/// to `state.corrupt-<time>.json` (unparseable), and the caller is told.
pub fn load(path: &Path) -> Result<Option<AppState>, LoadError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(LoadError::Unreadable(format!(
            "Не удалось прочитать сохранённые профили ({e}); изменения в этом сеансе не сохранятся"
        )))
        }
    };
    match serde_json::from_slice::<PersistedState>(&bytes) {
        Ok(persisted) => Ok(Some(persisted.into_app())),
        Err(e) => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = path.with_file_name(format!("state.corrupt-{stamp}.json"));
            let moved = std::fs::rename(path, &backup).is_ok();
            Err(LoadError::Corrupt(if moved {
                format!(
                    "Файл с профилями повреждён ({e}). Копия сохранена как {}",
                    backup.display()
                )
            } else {
                format!("Файл с профилями повреждён ({e})")
            }))
        }
    }
}

/// Writes atomically (temp file + rename), so a crash mid-write can't
/// leave a truncated file that loses every profile on the next launch.
/// The file holds share links with credentials: owner-only permissions.
pub fn save(path: &Path, app: &AppState) -> Result<(), String> {
    let json =
        serde_json::to_vec_pretty(&PersistedState::from_app(app)).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
    file.write_all(&json).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Group, Profile, UNGROUPED_ID};

    #[test]
    fn round_trips_library_but_not_runtime_status() {
        let dir = std::env::temp_dir().join(crate::state::unique_id("riekko-test-store"));
        let path = dir.join("state.json");
        let mut app = AppState::default();
        app.groups.push(Group {
            id: "g1".into(),
            name: "Work".into(),
        });
        app.profiles.push(Profile {
            id: "p1".into(),
            name: "NL".into(),
            endpoint: "nl.example.net:443".into(),
            transport: "TCP".into(),
            protocol: "VLESS".into(),
            group_id: "g1".into(),
            uri: Some("vless://uuid@nl.example.net:443".into()),
        });
        app.profiles.push(Profile {
            id: "p2".into(),
            name: "Orphan".into(),
            endpoint: "x:1".into(),
            transport: "TCP".into(),
            protocol: "VLESS".into(),
            group_id: "deleted-group".into(),
            uri: None,
        });
        app.active_profile_id = "p1".into();
        app.settings.auto_connect = true;
        app.state = crate::state::TunnelState::Connected;

        save(&path, &app).unwrap();
        let loaded = load(&path).ok().flatten().unwrap();
        assert_eq!(loaded.profiles.len(), 2);
        assert_eq!(loaded.groups[0].name, "Work");
        assert_eq!(loaded.active_profile_id, "p1");
        assert_eq!(loaded.session.endpoint, "nl.example.net:443");
        assert!(loaded.settings.auto_connect);
        assert!(loaded.state == crate::state::TunnelState::Idle);
        // A profile pointing at a group that no longer exists stays visible.
        assert_eq!(loaded.profiles[1].group_id, UNGROUPED_ID);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_a_first_launch_and_corrupt_one_is_kept_aside() {
        let dir = std::env::temp_dir().join(crate::state::unique_id("riekko-test-store"));
        assert!(matches!(load(&dir.join("state.json")), Ok(None)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(load(&path), Err(LoadError::Corrupt(_))));
        // The broken file was moved aside, not left to be overwritten.
        assert!(!path.exists());
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("state.corrupt-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(backups[0].path()).unwrap(), b"{not json");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
