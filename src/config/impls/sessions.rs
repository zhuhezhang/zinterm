use super::super::structs::*;
use super::normalize::*;

impl ConfigStore {
    pub fn sessions(&self) -> &[Session] {
        &self.cache.sessions
    }

    #[allow(dead_code)] // reserved for an upcoming reorder/drag-drop feature
    pub fn sessions_mut(&mut self) -> &mut Vec<Session> {
        &mut self.cache.sessions
    }

    /// Insert or replace a session. Assigns a `saved-…` id when empty, and
    /// always refreshes [`Session::saved_at`]. Returns the final session id.
    pub fn upsert(&mut self, mut session: Session) -> String {
        session.sanitize_for_kind();
        if session.id.trim().is_empty() {
            session.id = Session::new_saved_id();
        }
        session.saved_at = Session::now_saved_at();
        if is_reserved_session_group(session.group.trim()) {
            session.group.clear();
        }
        let exclude = Some(session.id.as_str());
        session.name =
            self.disambiguate_session_name(session.group.trim(), session.name.trim(), exclude);
        let id = session.id.clone();
        if let Some(existing) = self.cache.sessions.iter_mut().find(|s| s.id == session.id) {
            *existing = session;
        } else {
            self.cache.sessions.push(session);
        }
        id
    }

    pub fn remove(&mut self, id: &str) {
        self.cache.sessions.retain(|s| s.id != id);
        if let Ok(dir) = self.data_dir_path() {
            if let Err(e) = crate::config::vault::remove_secrets(&dir, id) {
                tracing::warn!("failed to remove vault secrets for {id}: {e:#}");
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.cache.sessions.iter().find(|s| s.id == id)
    }

    /// True when another session in `group` already uses `name` (exact match).
    pub fn session_name_taken_in_group(
        &self,
        group: &str,
        name: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        let group = normalize_session_group(group);
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        self.cache.sessions.iter().any(|s| {
            exclude_id.map(|id| s.id != id).unwrap_or(true)
                && normalize_session_group(&s.group) == group
                && s.name == name
        })
    }

    /// Keep `desired` when unique among sessions in `group`; else append
    /// `（1）`, `（2）`, … until free. `exclude_id` ignores that session (self).
    pub fn disambiguate_session_name(
        &self,
        group: &str,
        desired: &str,
        exclude_id: Option<&str>,
    ) -> String {
        let desired = desired.trim();
        if desired.is_empty() {
            return String::new();
        }
        if !self.session_name_taken_in_group(group, desired, exclude_id) {
            return desired.to_string();
        }
        for n in 1.. {
            let candidate = format!("{desired}（{n}）");
            if !self.session_name_taken_in_group(group, &candidate, exclude_id) {
                return candidate;
            }
        }
        unreachable!("unbounded session name suffix search");
    }

    /// All known group paths, including ancestors inferred from nested paths.
    pub(super) fn known_group_paths(&self) -> std::collections::BTreeSet<String> {
        let mut paths = std::collections::BTreeSet::new();
        let mut insert = |raw: &str| {
            let g = raw.trim();
            if g.is_empty() || is_reserved_session_group(g) {
                return;
            }
            paths.insert(g.to_string());
            let mut rest = g;
            while let Some(idx) = rest.rfind('/') {
                rest = &rest[..idx];
                if !rest.is_empty() {
                    paths.insert(rest.to_string());
                }
            }
        };
        for group in &self.cache.empty_groups {
            insert(group);
        }
        for session in &self.cache.sessions {
            insert(&session.group);
        }
        paths
    }

    /// Whether `path` is already a known group (excluding `exclude` full path).
    pub fn group_path_taken(&self, path: &str, exclude: Option<&str>) -> bool {
        let path = path.trim();
        if path.is_empty() {
            return false;
        }
        self.known_group_paths().iter().any(|g| {
            exclude
                .map(|e| !g.eq_ignore_ascii_case(e.trim()))
                .unwrap_or(true)
                && g.eq_ignore_ascii_case(path)
        })
    }

    /// Keep `desired` segment under `parent` when unique; else append `（1）`…
    pub fn disambiguate_group_segment(
        &self,
        parent: &str,
        desired: &str,
        exclude_full_path: Option<&str>,
    ) -> String {
        let desired = desired.trim();
        if desired.is_empty() {
            return String::new();
        }
        let parent = normalize_session_group(parent);
        let candidate = |seg: &str| group_join(&parent, seg);
        if !self.group_path_taken(&candidate(desired), exclude_full_path) {
            return desired.to_string();
        }
        for n in 1.. {
            let seg = format!("{desired}（{n}）");
            if !self.group_path_taken(&candidate(&seg), exclude_full_path) {
                return seg;
            }
        }
        unreachable!("unbounded group segment suffix search");
    }

    /// Move a session into `target_group` ("" = root). Renames on conflict.
    pub fn move_session_to_group(&mut self, id: &str, target_group: &str) -> bool {
        let target = normalize_session_group(target_group);
        let Some(idx) = self.cache.sessions.iter().position(|s| s.id == id) else {
            return false;
        };
        let current = normalize_session_group(&self.cache.sessions[idx].group);
        if current == target {
            return false;
        }
        let name = self.cache.sessions[idx].name.clone();
        let name = self.disambiguate_session_name(&target, &name, Some(id));
        self.cache.sessions[idx].group = target;
        self.cache.sessions[idx].name = name;
        true
    }

    /// Re-parent a group under `new_parent` ("" = top-level). Renames the
    /// segment on conflict. Rejects moves into self or a descendant.
    pub fn move_group_to_parent(&mut self, old: &str, new_parent: &str) -> bool {
        let old = old.trim();
        let new_parent = normalize_session_group(new_parent);
        if old.is_empty() || is_reserved_session_group(old) {
            return false;
        }
        if !new_parent.is_empty()
            && (new_parent.eq_ignore_ascii_case(old)
                || new_parent
                    .to_ascii_lowercase()
                    .starts_with(&format!("{}/", old.to_ascii_lowercase())))
        {
            return false;
        }
        if group_parent_path(old) == new_parent {
            return false;
        }
        let segment = group_path_segment(old).to_string();
        let segment = self.disambiguate_group_segment(&new_parent, &segment, Some(old));
        let new_full = group_join(&new_parent, &segment);
        if new_full.eq_ignore_ascii_case(old) {
            return false;
        }
        self.rename_group(old, new_full);
        true
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
    fn session_name_disambiguates_within_group_only() {
        let mut store = temp_store();
        let mut a = sample_session("web");
        a.id = "1".into();
        a.group = "prod".into();
        store.upsert(a);
        let mut b = sample_session("web");
        b.id = "2".into();
        b.group = "dev".into();
        store.upsert(b);

        assert_eq!(
            store.disambiguate_session_name("prod", "web", None),
            "web（1）"
        );
        assert_eq!(
            store.disambiguate_session_name("dev", "web", Some("2")),
            "web"
        );
        assert_eq!(store.disambiguate_session_name("other", "web", None), "web");

        let mut c = sample_session("web");
        c.id = "3".into();
        c.group = "prod".into();
        store.upsert(c);
        assert_eq!(store.get("3").unwrap().name, "web（1）");

        let mut d = sample_session("web");
        d.id = "4".into();
        d.group = "prod".into();
        store.upsert(d);
        assert_eq!(store.get("4").unwrap().name, "web（2）");
    }

    #[test]
    fn move_session_and_group_disambiguate_on_conflict() {
        let mut store = temp_store();
        store.add_group("prod".into());
        store.add_group("dev".into());
        store.add_group("dev/web".into());

        let mut a = sample_session("api");
        a.id = "s1".into();
        a.group = "prod".into();
        store.upsert(a);
        let mut b = sample_session("api");
        b.id = "s2".into();
        b.group = "dev".into();
        store.upsert(b);

        assert!(store.move_session_to_group("s1", "dev"));
        assert_eq!(store.get("s1").unwrap().group, "dev");
        assert_eq!(store.get("s1").unwrap().name, "api（1）");

        store.add_group("web".into());
        assert!(store.move_group_to_parent("web", "dev"));
        assert!(store.session_group_exists("dev/web（1）"));
        assert!(!store.session_group_exists("web"));

        // Cannot move a group into itself or a descendant.
        assert!(!store.move_group_to_parent("dev", "dev/web"));
        assert!(!store.move_group_to_parent("dev", "dev"));
    }
}
