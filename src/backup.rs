//! Sauvegardes automatiques de la base (portage de `utils/backup.py`).
//!
//! Utilise l'API de sauvegarde en ligne de SQLite, vérifie l'intégrité de la
//! copie, conserve les `MAX_BACKUPS` plus récentes et restaure automatiquement
//! la dernière sauvegarde saine si la base est corrompue.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OpenFlags};

use crate::bot::Data;
use crate::config;

pub const PREFIX: &str = "db_backup";

pub fn backup_dir(root: &Path) -> PathBuf {
    let dir = PathBuf::from(config::var("BACKUP_DIR").unwrap_or_else(|_| "backups".into()));
    if dir.is_absolute() {
        dir
    } else {
        root.join(dir)
    }
}

pub fn max_backups() -> usize {
    config::var("MAX_BACKUPS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(10)
        .clamp(1, 100)
}

/// `PRAGMA integrity_check` == "ok".
pub fn is_healthy(path: &Path) -> bool {
    let Ok(conn) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
        .map(|s| s == "ok")
        .unwrap_or(false)
}

pub fn copy_db(src: &Path, dest: &Path) -> Result<()> {
    let src = Connection::open(src)?;
    let mut dst = Connection::open(dest)?;
    let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
    backup.run_to_completion(100, Duration::from_millis(10), None)?;
    Ok(())
}

pub fn backups(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with(&format!("{prefix}_")) && n.ends_with(".sqlite3")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.into_iter().map(|(_, p)| p).collect()
}

/// Crée une sauvegarde vérifiée puis supprime les plus anciennes.
pub fn backup_database(db: &Path, dir: &Path, prefix: &str) -> Result<PathBuf> {
    anyhow::ensure!(db.exists(), "La base n'existe pas: {}", db.display());
    std::fs::create_dir_all(dir)?;
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dest = dir.join(format!("{prefix}_{ts}.sqlite3"));
    copy_db(db, &dest).context("sauvegarde SQLite")?;
    if !is_healthy(&dest) {
        let _ = std::fs::remove_file(&dest);
        anyhow::bail!("Échec de la vérification d'intégrité de la sauvegarde");
    }
    for old in backups(dir, prefix).into_iter().skip(max_backups()) {
        match std::fs::remove_file(&old) {
            Ok(()) => tracing::info!("Ancienne sauvegarde supprimée: {}", old.display()),
            Err(e) => tracing::warn!("Impossible de supprimer {}: {e}", old.display()),
        }
    }
    Ok(dest)
}

/// Restaure la base depuis la plus récente sauvegarde saine si elle est corrompue.
/// Renvoie `true` si la base est (ou redevient) saine.
pub fn auto_recover(db: &Path, dir: &Path) -> bool {
    if db.exists() && is_healthy(db) {
        return true;
    }
    tracing::warn!("Corruption de la base détectée: {}", db.display());
    for candidate in backups(dir, PREFIX) {
        if is_healthy(&candidate) {
            // Le fichier corrompu est mis de côté (SQLite ne peut pas l'ouvrir pour l'écraser).
            if db.exists() {
                let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let aside = PathBuf::from(format!("{}.corrupted_{ts}", db.display()));
                if let Err(e) = std::fs::rename(db, &aside) {
                    tracing::error!("Impossible de déplacer la base corrompue: {e}");
                    return false;
                }
                // Les fichiers WAL/SHM de l'ancienne base ne doivent pas être rejoués sur la copie.
                for ext in ["-wal", "-shm"] {
                    let side = PathBuf::from(format!("{}{ext}", db.display()));
                    if side.exists() {
                        let _ = std::fs::rename(&side, format!("{}{ext}", aside.display()));
                    }
                }
                tracing::warn!("Base corrompue conservée dans {}", aside.display());
            }
            match copy_db(&candidate, db) {
                Ok(()) => {
                    tracing::info!("Base restaurée depuis {}", candidate.display());
                    return true;
                }
                Err(e) => tracing::error!("Échec de la restauration: {e:#}"),
            }
        }
    }
    tracing::error!("Aucune sauvegarde valide trouvée pour la restauration");
    false
}

/// Toutes les 6 h : vérification d'intégrité + sauvegarde.
pub async fn scheduled_loop(data: Data) {
    crate::tasks::every("de sauvegarde automatique", None, Duration::from_secs(6 * 3600), || async {
        let db = data.db.path().to_path_buf();
        let dir = backup_dir(&data.config.root);
        let result = tokio::task::spawn_blocking(move || {
            if !auto_recover(&db, &dir) {
                tracing::error!(
                    "[Backup] ALERTE CRITIQUE : base corrompue et non récupérable ! Intervention manuelle requise."
                );
            }
            backup_database(&db, &dir, PREFIX)
        })
        .await;
        match result {
            Ok(Ok(p)) => tracing::info!("[Backup] Sauvegarde automatique créée: {}", p.display()),
            Ok(Err(e)) => tracing::warn!("[Backup] La sauvegarde automatique a échoué: {e:#}"),
            Err(e) => tracing::error!("[Backup] Tâche interrompue: {e}"),
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_verify_rotate_and_recover() {
        let dir = std::env::temp_dir().join(format!("isrobot-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("db.sqlite3");
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (42);")
            .unwrap();
        let bdir = dir.join("backups");
        let b = backup_database(&db, &bdir, "test").unwrap();
        assert!(is_healthy(&b));
        let x: i64 = Connection::open(&b)
            .unwrap()
            .query_row("SELECT x FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(x, 42);

        // Base corrompue → restauration depuis la sauvegarde « db_backup ».
        backup_database(&db, &bdir, PREFIX).unwrap();
        std::fs::write(&db, b"definitely not sqlite").unwrap();
        assert!(!is_healthy(&db));
        assert!(auto_recover(&db, &bdir));
        assert!(is_healthy(&db));
    }
}
