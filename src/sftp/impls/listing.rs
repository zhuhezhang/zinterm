use anyhow::{Context, Result};
use russh_sftp::client::SftpSession;

use crate::i18n::t;
use crate::ssh::{RemoteEntry, SessionEvent};

use super::tree::{build_tree_nodes, emit_tree};

/// A friendlier message for a failed directory listing, calling out the common
/// permission-denied case explicitly rather than dumping the raw error (#112).
pub(super) fn list_error_msg(path: &str, e: &impl std::fmt::Display) -> String {
    let raw = e.to_string();
    let low = raw.to_lowercase();
    if low.contains("permission") || low.contains("denied") {
        format!("{}: {}", t("权限不足,无法访问", "Permission denied"), path)
    } else {
        format!("{} {}: {}", t("无法访问", "Cannot open"), path, raw)
    }
}

pub(super) async fn list_dir_impl(sftp: &SftpSession, path: &str) -> Result<Vec<RemoteEntry>> {
    let raw = sftp
        .read_dir(path)
        .await
        .with_context(|| format!("read_dir {path} failed"))?;

    let mut entries: Vec<RemoteEntry> = raw
        .into_iter()
        .filter(|e| {
            let n = e.file_name();
            n != "." && n != ".."
        })
        .map(|e| {
            let name = e.file_name().to_string();
            let full_path = format!("{}/{}", path.trim_end_matches('/'), name);
            let meta = e.metadata();
            // Determine if entry is a directory via Unix permission bits.
            let permissions = meta.permissions.unwrap_or(0);
            let is_dir = (permissions & 0o170_000) == 0o040_000;
            let size = meta.size.unwrap_or(0);
            let modified = meta.mtime.unwrap_or(0);
            RemoteEntry {
                name,
                full_path,
                is_dir,
                size,
                modified,
                mode: permissions & 0o7777,
            }
        })
        .collect();

    // Sort: directories first, then files; both groups alphabetically.
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(entries)
}

/// List only the subdirectories of `path` (no files). Used to build the tree.
pub(super) async fn list_dirs_only_impl(
    sftp: &SftpSession,
    path: &str,
) -> Result<Vec<(String, String)>> {
    let entries = list_dir_impl(sftp, path).await?;
    Ok(entries
        .into_iter()
        .filter(|e| e.is_dir)
        .map(|e| (e.name, e.full_path))
        .collect())
}

#[allow(unused_variables)]
pub(super) async fn handle_list_dir(ctx: &mut super::ctx::WorkerCtx, path: String) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let _ = events.send(SessionEvent::SftpStatus(format!(
        "{} {}...",
        t("加载", "Loading"),
        path
    )));
    match list_dir_impl(&sftp, &path).await {
        Ok(entries) => {
            let _ = events.send(SessionEvent::SftpEntries {
                path: path.clone(),
                entries,
            });
            let _ = events.send(SessionEvent::SftpStatus(path));
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpError(list_error_msg(&path, &e)));
        }
    }
}

#[allow(unused_variables)]
pub(super) async fn handle_refresh_dir(ctx: &mut super::ctx::WorkerCtx, path: String) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let tree_dirs = &mut ctx.tree_dirs;
    let tree_expanded = &mut ctx.tree_expanded;
    // File panel — same as ListDir.
    let _ = events.send(SessionEvent::SftpStatus(format!(
        "{} {}...",
        t("加载", "Loading"),
        path
    )));
    match list_dir_impl(&sftp, &path).await {
        Ok(entries) => {
            let _ = events.send(SessionEvent::SftpEntries {
                path: path.clone(),
                entries,
            });
            let _ = events.send(SessionEvent::SftpStatus(path.clone()));
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpError(list_error_msg(&path, &e)));
        }
    }
    // Tree — re-fetch every currently-expanded directory so deleted /
    // created folders sync without a reconnect (#189). Stale entries
    // whose parent no longer lists them are simply never walked by
    // build_tree_nodes, so they drop out on the rebuild.
    let expanded: Vec<String> = tree_expanded.iter().cloned().collect();
    for dir in expanded {
        let dirs = list_dirs_only_impl(&sftp, &dir).await.unwrap_or_default();
        tree_dirs.insert(dir, dirs);
    }
    emit_tree(tree_dirs, tree_expanded, &events);
}

#[allow(unused_variables)]
pub(super) async fn handle_toggle_tree_node(ctx: &mut super::ctx::WorkerCtx, path: String) {
    let sftp = ctx.sftp.clone();
    let events = ctx.events.clone();
    let tree_dirs = &mut ctx.tree_dirs;
    let tree_expanded = &mut ctx.tree_expanded;
    if tree_expanded.contains(&path) {
        // Collapse this node and all descendants.
        let prefix = format!("{}/", path.trim_end_matches('/'));
        tree_expanded.retain(|p| p != &path && !p.starts_with(&prefix));
    } else {
        // Expand: fetch children if not yet cached.
        if !tree_dirs.contains_key(&path) {
            let dirs = list_dirs_only_impl(&sftp, &path).await.unwrap_or_default();
            tree_dirs.insert(path.clone(), dirs);
        }
        tree_expanded.insert(path.clone());
    }
    let mut nodes = Vec::new();
    build_tree_nodes("/", 0, tree_expanded, tree_dirs, &mut nodes);
    let _ = events.send(SessionEvent::SftpTreeUpdate(nodes));
}
