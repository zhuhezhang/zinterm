use super::super::structs::*;
use super::normalize::*;

impl ConfigStore {
    /// Delete every saved session and group folder (Quick Connect tree).
    /// When `clear_credentials` is true, also wipe the encrypted passwords /
    /// private-keys vault; otherwise vault entries are left in place (e.g. so a
    /// later restore of the same session ids can still unlock them).
    pub fn clear_sessions_and_groups(&mut self, clear_credentials: bool) {
        self.cache.sessions.clear();
        self.cache.empty_groups.clear();
        self.cache.collapsed_session_groups = None;
        if clear_credentials {
            if let Ok(dir) = self.data_dir_path() {
                if let Err(e) = crate::config::vault::clear_all(&dir) {
                    tracing::warn!("failed to clear credentials vault: {e:#}");
                }
            }
        }
    }

    // ── Session groups / folders (#41) ────────────────────────────────────

    /// Explicit groups (empty folders included). "default" is implicit.
    pub fn empty_groups(&self) -> &[String] {
        &self.cache.empty_groups
    }

    pub fn collapsed_session_groups(&self) -> Option<&[String]> {
        self.cache.collapsed_session_groups.as_deref()
    }

    /// Remember a Quick Connect folder's open/closed state. On the first
    /// interaction, materialise the default-collapsed state for every existing
    /// folder so expanding one folder does not accidentally expand the rest.
    pub fn set_session_group_collapsed(&mut self, name: &str, collapsed: bool) {
        self.ensure_collapsed_session_groups();

        let groups = self.cache.collapsed_session_groups.as_mut().unwrap();
        groups.retain(|group| group != name);
        if collapsed {
            groups.push(name.to_string());
            groups.sort();
            groups.dedup();
        }
    }

    /// Expand or collapse every Quick Connect folder at once.
    pub fn set_all_session_groups_collapsed(&mut self, collapsed: bool) {
        let all = self.collect_session_group_paths();
        self.cache.collapsed_session_groups = Some(if collapsed { all } else { Vec::new() });
    }

    /// Expand or collapse every descendant of `path`. When expanding, the
    /// folder itself is also opened so the children become visible; when
    /// collapsing, only descendants are closed so their headers stay shown.
    pub fn set_session_group_children_collapsed(&mut self, path: &str, collapsed: bool) {
        let path = path.trim();
        if path.is_empty() || is_reserved_session_group(path) {
            return;
        }
        let all = self.collect_session_group_paths();
        self.ensure_collapsed_session_groups();
        let prefix = format!("{path}/");
        let groups = self.cache.collapsed_session_groups.as_mut().unwrap();
        if collapsed {
            groups.retain(|group| group != path && !group.starts_with(&prefix));
            for group in &all {
                if group.starts_with(&prefix) {
                    groups.push(group.clone());
                }
            }
            groups.sort();
            groups.dedup();
        } else {
            groups.retain(|group| group != path && !group.starts_with(&prefix));
        }
    }

    pub(super) fn ensure_collapsed_session_groups(&mut self) {
        if self.cache.collapsed_session_groups.is_some() {
            return;
        }
        self.cache.collapsed_session_groups = Some(self.collect_session_group_paths());
    }

    pub(super) fn collect_session_group_paths(&self) -> Vec<String> {
        let mut groups = self.cache.empty_groups.clone();
        groups.extend(
            self.cache
                .sessions
                .iter()
                .filter(|session| {
                    let group = session.group.trim();
                    !group.is_empty() && !is_reserved_session_group(group)
                })
                .map(|session| session.group.clone()),
        );
        // Also materialise ancestor folders implied by nested paths.
        let nested: Vec<String> = groups
            .iter()
            .flat_map(|group| {
                let mut out = Vec::new();
                let mut rest = group.as_str();
                while let Some(idx) = rest.rfind('/') {
                    rest = &rest[..idx];
                    if !rest.is_empty() {
                        out.push(rest.to_string());
                    }
                }
                out
            })
            .collect();
        groups.extend(nested);
        groups.sort();
        groups.dedup();
        groups
    }

    /// Whether a user group already exists, including groups inferred from
    /// sessions that were created before explicit group records were added.
    pub fn session_group_exists(&self, name: &str) -> bool {
        let target = name.trim();
        if target.is_empty() {
            return false;
        }
        self.cache
            .empty_groups
            .iter()
            .any(|group| group.trim().eq_ignore_ascii_case(target))
            || self.cache.sessions.iter().any(|session| {
                !session.group.trim().is_empty()
                    && session.group.trim().eq_ignore_ascii_case(target)
            })
    }

    /// Create an empty group. Ignores blank/reserved names and duplicates.
    pub fn add_group(&mut self, name: String) {
        let n = name.trim().to_string();
        if n.is_empty() || is_reserved_session_group(&n) || self.session_group_exists(&n) {
            return;
        }
        self.cache.empty_groups.push(n.clone());
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            groups.push(n);
            groups.sort();
            groups.dedup();
        }
    }

    /// Keep a folder after its last session leaves. Groups that still contain a
    /// session are only inferred from `session.group`, so without this the
    /// folder vanishes as soon as that session is moved or deleted.
    /// Does not change collapse state (unlike [`Self::add_group`]).
    pub(super) fn retain_vacated_group(&mut self, group: &str) {
        let n = normalize_session_group(group);
        if n.is_empty() || self.session_group_exists(&n) {
            return;
        }
        self.cache.empty_groups.push(n);
    }

    /// Drop any `empty_groups` entry that now has a session in that folder or a
    /// descendant. Occupied folders are inferred from `session.group` alone.
    pub(super) fn prune_occupied_empty_groups(&mut self) {
        self.cache.empty_groups = self.collect_empty_groups();
    }

    /// Delete a group and cascade: nested child groups and all sessions in this
    /// group or any descendant are removed as well.
    pub fn remove_group(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() || is_reserved_session_group(name) {
            return;
        }
        let prefix = format!("{name}/");
        self.cache
            .empty_groups
            .retain(|g| g != name && !g.starts_with(&prefix));
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            groups.retain(|g| g != name && !g.starts_with(&prefix));
        }
        self.cache
            .sessions
            .retain(|s| s.group != name && !s.group.starts_with(&prefix));
    }

    /// Rename a group path, moving its sessions and nested descendants along.
    /// No-op for reserved names.
    pub fn rename_group(&mut self, old: &str, new: String) {
        let old = old.trim();
        let n = new.trim().to_string();
        if n.is_empty()
            || is_reserved_session_group(old)
            || is_reserved_session_group(&n)
            || n == old
            || (!n.eq_ignore_ascii_case(old) && self.session_group_exists(&n))
        {
            return;
        }
        let old_prefix = format!("{old}/");
        let new_prefix = format!("{n}/");
        for g in &mut self.cache.empty_groups {
            if g == old {
                *g = n.clone();
            } else if g.starts_with(&old_prefix) {
                *g = format!("{new_prefix}{}", g.strip_prefix(&old_prefix).unwrap_or(g));
            }
        }
        for s in &mut self.cache.sessions {
            if s.group == old {
                s.group = n.clone();
            } else if s.group.starts_with(&old_prefix) {
                s.group = format!(
                    "{new_prefix}{}",
                    s.group.strip_prefix(&old_prefix).unwrap_or(&s.group)
                );
            }
        }
        if let Some(groups) = &mut self.cache.collapsed_session_groups {
            for group in groups.iter_mut() {
                if group == old {
                    *group = n.clone();
                } else if group.starts_with(&old_prefix) {
                    *group = format!(
                        "{new_prefix}{}",
                        group.strip_prefix(&old_prefix).unwrap_or(group)
                    );
                }
            }
            groups.sort();
            groups.dedup();
        }
        self.cache.empty_groups.sort();
        self.cache.empty_groups.dedup();
    }
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::super::super::structs::*;
    use super::super::defaults_migration::{
        fresh_config, migrate_defaults, normalize_macos_renderer_mode,
        normalize_reserved_session_groups,
    };
    use super::super::test_support::{encrypt_export, sample_session, temp_store};

    #[test]
    fn quick_connect_groups_default_collapsed_and_remember_expansion() {
        let mut store = temp_store();
        store.cache.empty_groups = vec!["production".into(), "staging".into()];
        store.cache.sessions.push(Session {
            group: "production".into(),
            ..sample_session("server")
        });

        assert!(store.collapsed_session_groups().is_none());
        store.set_session_group_collapsed("production", false);

        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|group| group == "production"));
        assert!(collapsed.iter().any(|group| group == "staging"));
        assert!(!collapsed.iter().any(|group| group == "system"));
        assert!(!collapsed.iter().any(|group| group == "default"));

        store.set_session_group_collapsed("production", true);
        assert!(store
            .collapsed_session_groups()
            .unwrap()
            .iter()
            .any(|group| group == "production"));
    }

    #[test]
    fn expand_collapse_all_and_children_session_groups() {
        let mut store = temp_store();
        store.cache.empty_groups = vec![
            "prod".into(),
            "prod/web".into(),
            "prod/web/edge".into(),
            "staging".into(),
        ];

        store.set_all_session_groups_collapsed(false);
        assert!(store.collapsed_session_groups().unwrap().is_empty());

        store.set_all_session_groups_collapsed(true);
        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(collapsed.iter().any(|g| g == "prod"));
        assert!(collapsed.iter().any(|g| g == "prod/web"));
        assert!(collapsed.iter().any(|g| g == "prod/web/edge"));
        assert!(collapsed.iter().any(|g| g == "staging"));

        store.set_session_group_children_collapsed("prod", false);
        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|g| g == "prod"));
        assert!(!collapsed.iter().any(|g| g == "prod/web"));
        assert!(!collapsed.iter().any(|g| g == "prod/web/edge"));
        assert!(collapsed.iter().any(|g| g == "staging"));

        store.set_session_group_children_collapsed("prod", true);
        let collapsed = store.collapsed_session_groups().unwrap();
        assert!(!collapsed.iter().any(|g| g == "prod"));
        assert!(collapsed.iter().any(|g| g == "prod/web"));
        assert!(collapsed.iter().any(|g| g == "prod/web/edge"));
        assert!(collapsed.iter().any(|g| g == "staging"));
    }

    #[test]
    fn issue_316_reserved_system_groups_are_repaired_and_rejected() {
        let mut system_session = sample_session("misfiled");
        system_session.group = "system".into();
        system_session.password = Secret::default();
        let mut default_session = sample_session("legacy-default");
        default_session.group = "Default".into();
        let mut cfg = ConfigFile {
            sessions: vec![system_session, default_session],
            empty_groups: vec![
                "system".into(),
                "System".into(),
                "default".into(),
                "prod".into(),
            ],
            collapsed_session_groups: Some(vec!["system".into(), "prod".into()]),
            ..ConfigFile::default()
        };

        assert!(normalize_reserved_session_groups(&mut cfg));
        assert_eq!(cfg.empty_groups, ["prod"]);
        assert!(cfg.sessions.iter().all(|session| session.group.is_empty()));
        assert!(cfg.sessions[0].password.is_empty());
        // Collapse preferences for legacy reserved labels are display state and
        // may linger; normalization only repairs groups/sessions.
        assert_eq!(
            cfg.collapsed_session_groups.as_deref(),
            Some(["system".to_string(), "prod".to_string()].as_slice())
        );

        let mut store = temp_store();
        store.add_group("system".into());
        store.add_group("DEFAULT".into());
        store.add_group("prod".into());
        store.rename_group("prod", "System".into());
        assert_eq!(store.empty_groups(), ["prod"]);

        let mut session = sample_session("server");
        session.group = "SYSTEM".into();
        let id = store.upsert(session);
        assert_eq!(store.get(&id).unwrap().group, "");
    }

    #[test]
    fn session_group_names_are_unique_case_insensitively() {
        let mut store = temp_store();
        store.add_group("Production".into());
        store.add_group("production".into());
        assert_eq!(store.empty_groups(), ["Production"]);
        assert!(store.session_group_exists(" PRODUCTION "));

        let mut session = sample_session("staging-server");
        session.group = "Staging".into();
        store.upsert(session);
        assert!(store.session_group_exists("staging"));

        store.rename_group("Production", "STAGING".into());
        assert_eq!(store.empty_groups(), ["Production"]);

        // Changing only the spelling/case of the same group remains valid.
        store.rename_group("Production", "production".into());
        assert_eq!(store.empty_groups(), ["production"]);
    }
}
