use super::super::structs::*;
use super::defaults_migration::*;

impl ConfigStore {
    /// Saved quick commands (#55).
    pub fn quick_commands(&self) -> &[QuickCommand] {
        &self.cache.quick_commands
    }

    pub fn set_quick_commands(&mut self, cmds: Vec<QuickCommand>) {
        self.cache.quick_commands = cmds;
    }

    pub fn quick_panel_open(&self) -> bool {
        self.cache.quick_panel_open
    }

    pub fn quick_commands_as_sidebar(&self) -> bool {
        self.cache.quick_commands_as_sidebar
    }

    pub fn set_quick_commands_as_sidebar(&mut self, enabled: bool) {
        self.cache.quick_commands_as_sidebar = enabled;
        if !enabled {
            self.cache.quick_panel_open = false;
        }
    }

    pub fn set_quick_panel_open(&mut self, open: bool) {
        self.cache.quick_panel_open = open;
    }

    pub fn quick_panel_collapsed(&self) -> bool {
        self.cache.quick_panel_collapsed
    }

    pub fn set_quick_panel_collapsed(&mut self, collapsed: bool) {
        self.cache.quick_panel_collapsed = collapsed;
    }

    pub fn quick_panel_width(&self) -> f32 {
        let width = self.cache.quick_panel_width;
        if width <= 0.0 {
            default_quick_panel_width()
        } else {
            width
        }
    }

    pub fn set_quick_panel_width(&mut self, width: f32) {
        self.cache.quick_panel_width = width;
    }

    pub fn quick_panel_height(&self) -> f32 {
        let height = self.cache.quick_panel_height;
        if height <= 0.0 {
            default_quick_panel_height()
        } else {
            height
        }
    }

    pub fn set_quick_panel_height(&mut self, height: f32) {
        self.cache.quick_panel_height = height;
    }

    pub fn quick_panel_dock(&self) -> String {
        match self.cache.quick_panel_dock.trim() {
            "left" | "right" | "top" | "bottom" => self.cache.quick_panel_dock.clone(),
            _ => "right".into(),
        }
    }

    pub fn set_quick_panel_dock(&mut self, dock: String) {
        self.cache.quick_panel_dock = dock;
    }

    /// Explicit quick-command groups (#55) — parallels [`empty_groups`](Self::empty_groups).
    pub fn quick_empty_groups(&self) -> &[String] {
        &self.cache.quick_empty_groups
    }

    /// Create an empty quick-command group. Ignores blank, "default", duplicates.
    pub fn add_quick_group(&mut self, name: String) {
        let n = name.trim().to_string();
        if n.is_empty() || n.eq_ignore_ascii_case("default") {
            return;
        }
        if !self.cache.quick_empty_groups.iter().any(|g| g == &n) {
            self.cache.quick_empty_groups.push(n);
        }
    }

    /// Delete a quick-command group; any command still in it falls back to
    /// ungrouped (the UI only offers delete on empty groups, but clear defensively).
    pub fn remove_quick_group(&mut self, name: &str) {
        self.cache.quick_empty_groups.retain(|g| g != name);
        for c in &mut self.cache.quick_commands {
            if c.group == name {
                c.group.clear();
            }
        }
    }

    /// Rename a quick-command group, moving its commands along. No-op for
    /// blank / "default".
    pub fn rename_quick_group(&mut self, old: &str, new: String) {
        let n = new.trim().to_string();
        if n.is_empty() || n.eq_ignore_ascii_case("default") || n == old {
            return;
        }
        for g in &mut self.cache.quick_empty_groups {
            if g == old {
                *g = n.clone();
            }
        }
        for c in &mut self.cache.quick_commands {
            if c.group == old {
                c.group = n.clone();
            }
        }
        self.cache.quick_empty_groups.dedup();
    }

    /// Ordered named quick-command groups: explicit `quick_empty_groups` first, then
    /// any group referenced by a command that is not yet listed (first-seen order).
    pub fn materialized_quick_groups(&self) -> Vec<String> {
        let mut ordered: Vec<String> = self.cache.quick_empty_groups.clone();
        for c in &self.cache.quick_commands {
            let g = c.group.trim();
            if !g.is_empty() && !ordered.iter().any(|x| x == g) {
                ordered.push(g.to_string());
            }
        }
        ordered
    }

    /// Move a named quick-command group to sit immediately before `before` in the
    /// display order. `before` empty appends to the end. "default" cannot move.
    pub fn reorder_quick_group(&mut self, from: &str, before: &str) -> bool {
        if from.trim().is_empty() || from.eq_ignore_ascii_case("default") {
            return false;
        }
        let mut groups = self.materialized_quick_groups();
        let Some(from_idx) = groups.iter().position(|g| g == from) else {
            return false;
        };
        if before.is_empty() {
            let item = groups.remove(from_idx);
            groups.push(item);
        } else if before.eq_ignore_ascii_case("default") || from == before {
            return false;
        } else {
            let Some(mut to_idx) = groups.iter().position(|g| g == before) else {
                return false;
            };
            let item = groups.remove(from_idx);
            if to_idx > from_idx {
                to_idx -= 1;
            }
            groups.insert(to_idx, item);
        }
        self.cache.quick_empty_groups = groups;
        true
    }

    /// Update one quick command in place by index (#55).
    pub fn update_quick_command(&mut self, index: usize, cmd: QuickCommand) {
        if let Some(slot) = self.cache.quick_commands.get_mut(index) {
            *slot = cmd;
        }
    }

    /// Recent command-box history, oldest first (#55).
    pub fn command_history(&self) -> &[String] {
        &self.cache.command_history
    }

    /// Append a command to the history: skips blanks, de-duplicates globally so
    /// each command appears once, and re-appends at the end so the most-recently
    /// used command is always last. Capped so it can't grow without bound (#113).
    pub fn push_command_history(&mut self, cmd: String) {
        let cmd = repair_history_newlines(cmd);
        if cmd.trim().is_empty() {
            return;
        }
        // Drop any earlier occurrence, then push → no duplicates and "last used"
        // moves to the end (bash `HISTCONTROL=erasedups` semantics).
        self.cache.command_history.retain(|c| c != &cmd);
        const CAP: usize = 200;
        self.cache.command_history.push(cmd);
        let len = self.cache.command_history.len();
        if len > CAP {
            self.cache.command_history.drain(0..len - CAP);
        }
    }

    /// Remove a single command-history entry by storage index (#96).
    pub fn remove_command_history(&mut self, index: usize) {
        if index < self.cache.command_history.len() {
            self.cache.command_history.remove(index);
        }
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
    fn commands_file_roundtrip_keeps_quick_commands_and_history() {
        let mut store = temp_store();
        store.set_quick_commands(vec![crate::config::QuickCommand {
            name: "ll".into(),
            command: "ls -la".into(),
            group: "ops".into(),
            send_enter: true,
        }]);
        store.add_quick_group("ops".into());
        store.cache.command_history = vec!["echo hi".into()];
        store.save().unwrap();

        let dir = store.path.parent().unwrap().to_path_buf();
        let commands_path = dir.join("commands.json");
        assert!(commands_path.exists());
        let commands_raw = std::fs::read_to_string(&commands_path).unwrap();
        assert!(commands_raw.contains("ls -la"));
        assert!(commands_raw.contains("echo hi"));
        assert!(commands_raw.contains("\"quick_empty_groups\""));
        assert!(!commands_raw.contains("\"quick_groups\""));
        let sessions_raw = std::fs::read_to_string(&store.path).unwrap();
        assert!(!sessions_raw.contains("quick_commands"));
        assert!(!sessions_raw.contains("command_history"));

        let reloaded = {
            let settings_path = crate::config::persist::settings_path(&dir);
            let ui_path = crate::config::persist::ui_state_path(&dir);
            let mut s = ConfigStore {
                path: store.path.clone(),
                cache: ConfigFile::default(),
            };
            s.cache =
                ConfigStore::load_split(&store.path, &commands_path, &settings_path, &ui_path)
                    .unwrap();
            s
        };
        assert_eq!(reloaded.quick_commands().len(), 1);
        assert_eq!(reloaded.quick_commands()[0].name, "ll");
        assert!(reloaded.quick_empty_groups().iter().any(|g| g == "ops"));
        assert_eq!(reloaded.command_history(), &["echo hi".to_string()]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn quick_empty_groups_keep_user_order_and_reorder() {
        let mut store = temp_store();
        store.add_quick_group("beta".into());
        store.add_quick_group("alpha".into());
        store.set_quick_commands(vec![crate::config::QuickCommand {
            name: "x".into(),
            command: "x".into(),
            group: "gamma".into(),
            send_enter: true,
        }]);
        assert_eq!(
            store.materialized_quick_groups(),
            vec!["beta", "alpha", "gamma"]
        );
        assert!(store.reorder_quick_group("gamma", "beta"));
        assert_eq!(
            store.materialized_quick_groups(),
            vec!["gamma", "beta", "alpha"]
        );
        assert!(store.reorder_quick_group("beta", ""));
        assert_eq!(
            store.materialized_quick_groups(),
            vec!["gamma", "alpha", "beta"]
        );
    }
}
