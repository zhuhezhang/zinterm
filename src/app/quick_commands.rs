use super::*;

use super::terminal_ui::history_summary;

fn groups_match(a: &str, b: &str) -> bool {
    a.trim() == b.trim()
}

fn name_used_in_group(
    commands: &[crate::config::QuickCommand],
    group: &str,
    name: &str,
    exclude: Option<usize>,
) -> bool {
    commands.iter().enumerate().any(|(i, c)| {
        exclude != Some(i) && groups_match(&c.group, group) && c.name == name
    })
}

/// Pick `name(1)`, `name(2)`, … first suffix not used in `group`.
pub(super) fn duplicate_quick_command_name(
    commands: &[crate::config::QuickCommand],
    group: &str,
    source_name: &str,
) -> String {
    for n in 1.. {
        let candidate = format!("{source_name}({n})");
        if !name_used_in_group(commands, group, &candidate, None) {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search")
}

/// Keep `desired` when unique in `group` (optionally ignoring one index); else
/// append `(1)`, `(2)`, … like [`duplicate_quick_command_name`].
pub(super) fn disambiguate_quick_command_name(
    commands: &[crate::config::QuickCommand],
    group: &str,
    desired: &str,
    exclude: Option<usize>,
) -> String {
    if !name_used_in_group(commands, group, desired, exclude) {
        return desired.to_string();
    }
    for n in 1.. {
        let candidate = format!("{desired}({n})");
        if !name_used_in_group(commands, group, &candidate, exclude) {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search")
}

pub(super) fn all_quick_group_names(store: &ConfigStore) -> std::collections::HashSet<String> {
    let cmds = store.quick_commands();
    let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
    if cmds.iter().any(|c| c.group.trim().is_empty()) {
        set.insert("default".to_string());
    }
    for g in store.quick_groups() {
        set.insert(g.clone());
    }
    for c in cmds {
        let g = c.group.trim();
        if !g.is_empty() {
            set.insert(g.to_string());
        }
    }
    set
}

/// Build the quick-command model for the command bar + manage dialog (#55).
///
/// Grouped like the welcome session list: the implicit "default" group (entries
/// with an empty group) comes first, then named groups alphabetically. Within a
/// group, entries keep their saved order. `group_header` is set on the first row
/// of each group; `collapsed` reflects `collapsed_groups` (runtime-only state);
/// `orig_index` points back into the stored vec so deletes target the right entry
/// even though the display order differs.
pub(super) fn quick_cmd_rows(
    store: &ConfigStore,
    collapsed_groups: &std::collections::HashSet<String>,
) -> Vec<QuickCmd> {
    let cmds = store.quick_commands();

    let has_default = cmds.iter().any(|c| c.group.trim().is_empty());
    let named = store.materialized_quick_groups();

    let mut groups: Vec<String> = Vec::new();
    if has_default {
        groups.push("default".to_string());
    }
    groups.extend(named);

    let mut rows: Vec<QuickCmd> = Vec::new();
    for group in &groups {
        let is_collapsed = collapsed_groups.contains(group);
        let members: Vec<(usize, &crate::config::QuickCommand)> = cmds
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let g = c.group.trim();
                if group == "default" {
                    g.is_empty()
                } else {
                    g == group
                }
            })
            .collect();
        if members.is_empty() {
            // Header-only placeholder for an empty group (orig_index -1) so it can
            // still be renamed / deleted, matching empty session folders.
            rows.push(QuickCmd {
                name: "".into(),
                command: "".into(),
                summary: "".into(),
                group: group.clone().into(),
                group_header: group.clone().into(),
                collapsed: is_collapsed,
                orig_index: -1,
            });
        } else {
            for (i, (orig_idx, c)) in members.iter().enumerate() {
                rows.push(QuickCmd {
                    name: c.name.clone().into(),
                    command: c.command.clone().into(),
                    summary: history_summary(&c.command).into(),
                    group: group.clone().into(),
                    group_header: if i == 0 {
                        group.clone().into()
                    } else {
                        "".into()
                    },
                    collapsed: is_collapsed,
                    orig_index: *orig_idx as i32,
                });
            }
        }
    }
    rows
}

pub(super) fn quick_cmds_from_model(model: &ModelRc<QuickCmd>) -> Vec<QuickCmd> {
    (0..model.row_count())
        .filter_map(|i| model.row_data(i))
        .collect()
}

pub(super) fn quick_cmd_model(
    store: &ConfigStore,
    collapsed_groups: &std::collections::HashSet<String>,
) -> ModelRc<QuickCmd> {
    ModelRc::from(Rc::new(VecModel::from(quick_cmd_rows(store, collapsed_groups))))
}

fn quick_cmd_matches(cmd: &QuickCmd, q: &str) -> bool {
    cmd.name.to_lowercase().contains(q) || cmd.command.to_lowercase().contains(q)
}

/// Filtered quick-command rows for the command-bar popup: case-insensitive
/// substring matches on name or command text; groups with no matches are hidden
/// and matching groups are shown expanded.
pub(super) fn quick_cmd_view_model(
    store: &ConfigStore,
    collapsed_groups: &std::collections::HashSet<String>,
    query: &str,
) -> ModelRc<QuickCmd> {
    let rows = quick_cmd_rows(store, collapsed_groups);
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return ModelRc::from(Rc::new(VecModel::from(rows)));
    }

    let matching_groups: std::collections::HashSet<String> = rows
        .iter()
        .filter(|r| r.orig_index >= 0 && quick_cmd_matches(r, &q))
        .map(|r| r.group.to_string())
        .collect();

    let mut out = Vec::new();
    let mut last_group = String::new();
    let mut header_emitted = false;

    for row in rows {
        let group = row.group.to_string();
        if group != last_group {
            last_group = group.clone();
            header_emitted = false;
        }
        if !matching_groups.contains(&group) {
            continue;
        }
        if row.orig_index < 0 || !quick_cmd_matches(&row, &q) {
            continue;
        }
        let mut item = row;
        if !header_emitted {
            item.group_header = group.into();
            header_emitted = true;
        } else {
            item.group_header = "".into();
        }
        item.collapsed = false;
        out.push(item);
    }

    ModelRc::from(Rc::new(VecModel::from(out)))
}

/// Drag-drop in the manage dialog: move `from` before `before_orig` in the stored
/// list (`before_orig < 0` = append to `target_group`). `target_group` uses the
/// display name `"default"` for the implicit empty group.
pub(super) fn drop_quick_command(
    commands: &mut Vec<crate::config::QuickCommand>,
    from: usize,
    target_group: &str,
    before_orig: i32,
) -> bool {
    if from >= commands.len() {
        return false;
    }
    let stored_group = if target_group == "default" {
        String::new()
    } else {
        target_group.to_string()
    };

    let mut cmd = commands.remove(from);
    cmd.group = stored_group.clone();
    cmd.name = disambiguate_quick_command_name(commands, &stored_group, &cmd.name, None);

    let insert_at = if before_orig < 0 {
        let mut last: Option<usize> = None;
        for (i, c) in commands.iter().enumerate() {
            if c.group.trim() == stored_group.trim() {
                last = Some(i);
            }
        }
        last.map(|i| i + 1).unwrap_or(commands.len())
    } else {
        let before = before_orig as usize;
        let at = if before > from { before - 1 } else { before };
        at.min(commands.len())
    };

    commands.insert(insert_at, cmd);
    true
}

/// Layout metrics for the manage-dialog command list (`ui/app.slint`).
const QCM_GROUP_HDR: f32 = 26.0;
const QCM_CMD_ROW: f32 = 38.0;
const QCM_ROW_GAP: f32 = 2.0;
const QCM_APPEND_ZONE: f32 = 10.0;

pub(super) struct QcmCommandDrop {
    pub group: String,
    pub before_orig: i32,
}

/// Hit-test the manage-dialog list while dragging a command.
pub(super) fn qcm_command_drop_at(
    rows: &[QuickCmd],
    list_top: f32,
    pointer_y: f32,
) -> Option<QcmCommandDrop> {
    let mut y = list_top;
    for row in rows {
        let mut row_h = 0.0f32;
        if !row.group_header.is_empty() {
            if (y..y + QCM_GROUP_HDR).contains(&pointer_y) {
                return Some(QcmCommandDrop {
                    group: row.group.to_string(),
                    before_orig: -1,
                });
            }
            row_h += QCM_GROUP_HDR;
        }
        if row.orig_index >= 0 && !row.collapsed {
            if (y + row_h..y + row_h + QCM_CMD_ROW).contains(&pointer_y) {
                return Some(QcmCommandDrop {
                    group: row.group.to_string(),
                    before_orig: row.orig_index,
                });
            }
            row_h += QCM_CMD_ROW;
        }
        y += row_h;
        if row_h > 0.0 {
            y += QCM_ROW_GAP;
        }
    }
    if pointer_y >= y && pointer_y <= y + QCM_APPEND_ZONE {
        rows.iter()
            .rev()
            .find_map(|row| {
                if row.group.is_empty() {
                    None
                } else {
                    Some(row.group.to_string())
                }
            })
            .map(|group| QcmCommandDrop {
                group,
                before_orig: -1,
            })
    } else {
        None
    }
}

/// Hit-test the manage-dialog list while dragging a group header.
pub(super) fn qcm_group_drop_at(rows: &[QuickCmd], list_top: f32, pointer_y: f32) -> String {
    let mut y = list_top;
    for row in rows {
        let mut row_h = 0.0f32;
        if !row.group_header.is_empty() {
            if (y..y + QCM_GROUP_HDR).contains(&pointer_y) {
                return row.group.to_string();
            }
            row_h += QCM_GROUP_HDR;
        }
        if row.orig_index >= 0 && !row.collapsed {
            row_h += QCM_CMD_ROW;
        }
        y += row_h;
        if row_h > 0.0 {
            y += QCM_ROW_GAP;
        }
    }
    if pointer_y >= y && pointer_y <= y + QCM_APPEND_ZONE {
        String::new()
    } else {
        // Keep the last hover target when the pointer leaves the list.
        rows.iter()
            .rev()
            .find_map(|row| {
                if row.group_header.is_empty() {
                    None
                } else {
                    Some(row.group.to_string())
                }
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod drop_tests {
    use super::{
        disambiguate_quick_command_name, drop_quick_command, duplicate_quick_command_name,
        qcm_command_drop_at, QuickCmd,
    };
    use crate::config::QuickCommand;

    fn command(name: &str, group: &str) -> QuickCommand {
        QuickCommand {
            name: name.to_string(),
            command: name.to_string(),
            group: group.to_string(),
            send_enter: true,
        }
    }

    #[test]
    fn duplicate_name_uses_incrementing_suffix() {
        let cmds = vec![command("foo", "g"), command("foo(1)", "g")];
        assert_eq!(duplicate_quick_command_name(&cmds, "g", "foo"), "foo(2)");
        assert_eq!(duplicate_quick_command_name(&cmds, "g", "bar"), "bar(1)");
    }

    #[test]
    fn disambiguate_skips_self_and_renames_on_conflict() {
        let cmds = vec![command("foo", "g"), command("bar", "g")];
        assert_eq!(
            disambiguate_quick_command_name(&cmds, "g", "foo", Some(0)),
            "foo"
        );
        assert_eq!(
            disambiguate_quick_command_name(&cmds, "g", "bar", Some(0)),
            "bar(1)"
        );
    }

    #[test]
    fn drop_renames_when_target_group_has_same_name() {
        let mut commands = vec![command("foo", "ops"), command("foo", "other")];
        assert!(drop_quick_command(&mut commands, 0, "other", 0));
        assert_eq!(
            commands
                .iter()
                .map(|item| (item.name.as_str(), item.group.as_str()))
                .collect::<Vec<_>>(),
            vec![("foo(1)", "other"), ("foo", "other")]
        );
    }

    #[test]
    fn drop_reorders_within_a_group() {
        let mut commands = vec![
            command("a", "ops"),
            command("b", "ops"),
            command("c", "ops"),
        ];
        assert!(drop_quick_command(&mut commands, 2, "ops", 0));
        assert_eq!(
            commands
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "a", "b"]
        );
    }

    #[test]
    fn drop_moves_into_another_group_before_target() {
        let mut commands = vec![
            command("a", "ops"),
            command("x", "other"),
            command("b", "ops"),
        ];
        assert!(drop_quick_command(&mut commands, 2, "other", 1));
        assert_eq!(
            commands
                .iter()
                .map(|item| (item.name.as_str(), item.group.as_str()))
                .collect::<Vec<_>>(),
            vec![("a", "ops"), ("b", "other"), ("x", "other")]
        );
    }

    #[test]
    fn drop_on_group_header_appends_to_that_group() {
        let mut commands = vec![command("a", "ops"), command("b", "other")];
        assert!(drop_quick_command(&mut commands, 0, "other", -1));
        assert_eq!(
            commands
                .iter()
                .map(|item| (item.name.as_str(), item.group.as_str()))
                .collect::<Vec<_>>(),
            vec![("b", "other"), ("a", "other")]
        );
    }

    fn row(group: &str, header: bool, orig: i32) -> QuickCmd {
        QuickCmd {
            name: if orig >= 0 { "cmd".into() } else { "".into() },
            command: "".into(),
            summary: "".into(),
            group: group.into(),
            group_header: if header { group.into() } else { "".into() },
            collapsed: false,
            orig_index: orig,
        }
    }

    #[test]
    fn command_drop_hit_test_finds_group_header_and_row() {
        let rows = vec![
            row("ops", true, 0),
            row("ops", false, 1),
            row("other", true, 2),
        ];
        let top = 100.0;
        let hdr = qcm_command_drop_at(&rows, top, top + 10.0).unwrap();
        assert_eq!(hdr.group, "ops");
        assert_eq!(hdr.before_orig, -1);
        let cmd = qcm_command_drop_at(&rows, top, top + 26.0 + 10.0).unwrap();
        assert_eq!(cmd.group, "ops");
        assert_eq!(cmd.before_orig, 0);
        let second = qcm_command_drop_at(&rows, top, top + 26.0 + 38.0 + 2.0 + 10.0).unwrap();
        assert_eq!(second.before_orig, 1);
    }
}
