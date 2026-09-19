use anyhow::Result;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::FileAttributes;

use crate::i18n::t;
use crate::ssh::SessionEvent;

use super::listing::list_dir_impl;
use super::paths::{base_name, parent_dir};
use super::tree::{emit_tree, sync_tree_dir};

/// Recursively remove a remote directory tree (#50 follow-up).
///
/// A plain `remove_dir` only deletes an *empty* directory, so deleting an
/// uploaded folder failed. We BFS to discover every sub-directory (deleting
/// files as we go), then rmdir them deepest-first.
pub(super) async fn remove_dir_recursive(sftp: &SftpSession, root: &str) -> Result<()> {
    let mut all_dirs = vec![root.trim_end_matches('/').to_string()];
    let mut i = 0;
    while i < all_dirs.len() {
        let d = all_dirs[i].clone();
        i += 1;
        for entry in list_dir_impl(sftp, &d).await? {
            if entry.is_dir {
                all_dirs.push(entry.full_path);
            } else {
                sftp.remove_file(&entry.full_path)
                    .await
                    .map_err(|e| anyhow::anyhow!("remove file {}: {e}", entry.full_path))?;
            }
        }
    }
    // BFS discovered parents before children, so reversing gives deepest-first.
    for d in all_dirs.iter().rev() {
        sftp.remove_dir(d)
            .await
            .map_err(|e| anyhow::anyhow!("remove dir {d}: {e}"))?;
    }
    Ok(())
}

#[allow(unused_variables)]
pub(super) async fn handle_delete(ctx: &mut super::ctx::WorkerCtx, path: String) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let tree_dirs = &mut ctx.tree_dirs;
    let tree_expanded = &mut ctx.tree_expanded;
    let filename = base_name(&path);
    let _ = events.send(SessionEvent::SftpStatus(format!(
        "{} {}...",
        t("删除", "Deleting"),
        filename
    )));
    // Directories are removed recursively (a plain remove_dir only
    // works on an empty dir, so an uploaded folder couldn't be
    // deleted); files via remove_file.
    let is_dir = sftp
        .metadata(&path)
        .await
        .ok()
        .map(|m| (m.permissions.unwrap_or(0) & 0o170_000) == 0o040_000)
        .unwrap_or(false);
    let res: Result<()> = if is_dir {
        remove_dir_recursive(&sftp, &path).await
    } else {
        sftp.remove_file(&path)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("{e}"))
    };
    match res {
        Ok(_) => {
            let parent = parent_dir(&path);
            if let Ok(entries) = list_dir_impl(&sftp, &parent).await {
                let _ = events.send(SessionEvent::SftpEntries {
                    path: parent.clone(),
                    entries,
                });
            }
            // Keep the left directory tree in sync (#189): drop the
            // deleted folder and any cached descendants, then re-list
            // the parent's sub-dirs so the deleted node disappears
            // without needing a reconnect.
            let prefix = format!("{}/", path.trim_end_matches('/'));
            tree_dirs.retain(|p, _| p != &path && !p.starts_with(&prefix));
            tree_expanded.retain(|p| p != &path && !p.starts_with(&prefix));
            sync_tree_dir(&sftp, &parent, tree_dirs).await;
            emit_tree(tree_dirs, tree_expanded, &events);
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {}",
                t("已删除", "Deleted"),
                filename
            )));
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {e}",
                t("删除失败", "Delete failed")
            )));
        }
    }
}

#[allow(unused_variables)]
pub(super) async fn handle_rename(ctx: &mut super::ctx::WorkerCtx, from: String, to: String) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let tree_dirs = &mut ctx.tree_dirs;
    let tree_expanded = &mut ctx.tree_expanded;
    let refresh = parent_dir(&from);
    match sftp.rename(&from, &to).await {
        Ok(_) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {}",
                t("已重命名", "Renamed"),
                base_name(&to)
            )));
            // Sync the left tree (#189): drop the old name + cached
            // descendants, then re-list both the source and the
            // destination parent (rename can also move across dirs).
            let prefix = format!("{}/", from.trim_end_matches('/'));
            tree_dirs.retain(|p, _| p != &from && !p.starts_with(&prefix));
            tree_expanded.retain(|p| p != &from && !p.starts_with(&prefix));
            sync_tree_dir(&sftp, &refresh, tree_dirs).await;
            let to_parent = parent_dir(&to);
            if to_parent != refresh {
                sync_tree_dir(&sftp, &to_parent, tree_dirs).await;
            }
            emit_tree(tree_dirs, tree_expanded, &events);
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {e}",
                t("重命名失败", "Rename failed")
            )));
        }
    }
    if let Ok(entries) = list_dir_impl(&sftp, &refresh).await {
        let _ = events.send(SessionEvent::SftpEntries {
            path: refresh,
            entries,
        });
    }
}

#[allow(unused_variables)]
pub(super) async fn handle_chmod(ctx: &mut super::ctx::WorkerCtx, path: String, mode: u32) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let refresh = parent_dir(&path);
    let attrs = FileAttributes {
        permissions: Some(mode),
        ..Default::default()
    };
    match sftp.set_metadata(&path, attrs).await {
        Ok(_) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {} → {:o}",
                t("已修改权限", "Permissions changed"),
                base_name(&path),
                mode
            )));
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {e}",
                t("修改权限失败", "chmod failed")
            )));
        }
    }
    if let Ok(entries) = list_dir_impl(&sftp, &refresh).await {
        let _ = events.send(SessionEvent::SftpEntries {
            path: refresh,
            entries,
        });
    }
}

#[allow(unused_variables)]
pub(super) async fn handle_mkdir(ctx: &mut super::ctx::WorkerCtx, path: String) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let tree_dirs = &mut ctx.tree_dirs;
    let tree_expanded = &mut ctx.tree_expanded;
    let refresh = parent_dir(&path);
    match sftp.create_dir(&path).await {
        Ok(_) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {}",
                t("已新建文件夹", "Folder created"),
                base_name(&path)
            )));
            // Show the new folder in the left tree too (#189).
            sync_tree_dir(&sftp, &refresh, tree_dirs).await;
            emit_tree(tree_dirs, tree_expanded, &events);
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpStatus(format!(
                "{}: {e}",
                t("新建文件夹失败", "Create folder failed")
            )));
        }
    }
    if let Ok(entries) = list_dir_impl(&sftp, &refresh).await {
        let _ = events.send(SessionEvent::SftpEntries {
            path: refresh,
            entries,
        });
    }
}

#[allow(unused_variables)]
pub(super) async fn handle_touch(ctx: &mut super::ctx::WorkerCtx, path: String) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let refresh = parent_dir(&path);
    // create() truncates if the file exists, so refuse to clobber.
    let exists = sftp.metadata(&path).await.is_ok();
    if exists {
        let _ = events.send(SessionEvent::SftpStatus(format!(
            "{}: {}",
            t("文件已存在", "File already exists"),
            base_name(&path)
        )));
    } else {
        match sftp.create(&path).await {
            Ok(_) => {
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{}: {}",
                    t("已新建文件", "File created"),
                    base_name(&path)
                )));
            }
            Err(e) => {
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{}: {e}",
                    t("新建文件失败", "Create file failed")
                )));
            }
        }
    }
    if let Ok(entries) = list_dir_impl(&sftp, &refresh).await {
        let _ = events.send(SessionEvent::SftpEntries {
            path: refresh,
            entries,
        });
    }
}
