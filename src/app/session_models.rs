use std::collections::{BTreeMap, BTreeSet};

use super::*;

fn session_endpoint(session: &Session) -> String {
    match session.kind {
        SessionKind::Serial => session.serial_port.clone(),
        SessionKind::Local => {
            let shell = session.shell.trim();
            if !shell.is_empty() {
                shell.to_string()
            } else {
                session.working_directory.trim().to_string()
            }
        }
        _ => session.host.clone(),
    }
}

#[derive(Default)]
struct GroupTreeNode {
    full_path: String,
    children: BTreeMap<String, GroupTreeNode>,
}

fn collect_user_group_paths(store: &ConfigStore) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for group in store.groups() {
        let group = group.trim();
        if group.is_empty() || is_reserved_session_group(group) {
            continue;
        }
        paths.insert(group.to_string());
        for ancestor in ancestor_paths(group) {
            paths.insert(ancestor);
        }
    }
    for session in store.sessions() {
        let group = session.group.trim();
        if group.is_empty() || is_reserved_session_group(group) {
            continue;
        }
        paths.insert(group.to_string());
        for ancestor in ancestor_paths(group) {
            paths.insert(ancestor);
        }
    }
    paths
}

fn ancestor_paths(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(idx) = rest.rfind('/') {
        rest = &rest[..idx];
        out.push(rest.to_string());
    }
    out
}

fn build_user_group_tree(paths: &BTreeSet<String>) -> BTreeMap<String, GroupTreeNode> {
    let mut roots = BTreeMap::new();
    for path in paths {
        let mut built = String::new();
        let mut current = &mut roots;
        for (i, segment) in path.split('/').enumerate() {
            if segment.is_empty() {
                continue;
            }
            if i > 0 {
                built.push('/');
            }
            built.push_str(segment);
            let node = current
                .entry(segment.to_string())
                .or_insert_with(|| GroupTreeNode {
                    full_path: built.clone(),
                    children: BTreeMap::new(),
                });
            current = &mut node.children;
        }
    }
    roots
}

fn group_depth(path: &str) -> i32 {
    path.matches('/').count() as i32
}

fn ordered_user_group_paths(store: &ConfigStore) -> Vec<String> {
    let paths = collect_user_group_paths(store);
    let roots = build_user_group_tree(&paths);
    let mut out = Vec::new();
    fn walk(nodes: &BTreeMap<String, GroupTreeNode>, out: &mut Vec<String>) {
        for node in nodes.values() {
            out.push(node.full_path.clone());
            walk(&node.children, out);
        }
    }
    walk(&roots, &mut out);
    out
}

pub(super) fn parse_batch_import(text: &str) -> Vec<Session> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // splitn(5) so the last field (name) may itself contain '|'.
        let parts: Vec<&str> = line.splitn(5, '|').map(str::trim).collect();
        let host = parts.first().copied().unwrap_or("");
        // Skip blank hosts and a header row like "host|port|username|...".
        if host.is_empty() || host.eq_ignore_ascii_case("host") {
            continue;
        }
        let port = parts
            .get(1)
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|&p| p > 0)
            .unwrap_or(22);
        let user = parts
            .get(2)
            .copied()
            .filter(|s| !s.is_empty())
            .unwrap_or("root");
        let password = parts.get(3).copied().unwrap_or("");
        let name = parts
            .get(4)
            .copied()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{user}@{host}"));
        let mut sess = Session {
            name,
            host: host.to_string(),
            port,
            user: user.to_string(),
            auth: AuthMethod::Password,
            ..Session::new_empty()
        };
        if !password.is_empty() {
            sess.password = Secret::new(password.to_string());
        }
        out.push(sess);
    }
    out
}

/// Distinct named groups in tree order — feeds the new/edit dialog's group
/// dropdown (#179). Ungrouped ("") is excluded; the dialog leaves the field blank
/// for that case (root of the Quick Connect tree).
pub(super) fn session_groups_model(store: &ConfigStore) -> ModelRc<SharedString> {
    ModelRc::from(Rc::new(VecModel::from(
        ordered_user_group_paths(store)
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    )))
}

pub(super) fn sync_sessions_to_model(store: &ConfigStore, model: &VecModel<SessionInfo>) {
    let sessions = store.sessions();
    let collapsed_groups = store.collapsed_session_groups();
    let group_is_collapsed = |group: &str| {
        collapsed_groups
            .map(|groups| groups.iter().any(|collapsed| collapsed == group))
            .unwrap_or(true)
    };

    let blank = |group: &str| SessionInfo {
        id: "".into(),
        name: "".into(),
        host: "".into(),
        port: 0,
        user: "".into(),
        auth: "".into(),
        last_used: "".into(),
        group: group.into(),
        group_header: group.into(),
        group_label: group_path_segment(group).into(),
        group_depth: group_depth(group),
        collapsed: group_is_collapsed(group),
        builtin: false,
        conn_kind: "".into(),
        endpoint: "".into(),
    };

    let mut rows: Vec<SessionInfo> = Vec::new();

    // Groups first (tree order, ascending), then ungrouped root sessions.
    let user_tree = build_user_group_tree(&collect_user_group_paths(store));

    fn emit_group_branch(
        sessions: &[Session],
        node: &GroupTreeNode,
        rows: &mut Vec<SessionInfo>,
        group_is_collapsed: &impl Fn(&str) -> bool,
        blank: &impl Fn(&str) -> SessionInfo,
    ) {
        let group = node.full_path.as_str();
        if ancestor_collapsed(group, group_is_collapsed) {
            return;
        }
        let collapsed = group_is_collapsed(group);
        let depth = group_depth(group);
        let label = group_path_segment(group);

        let mut direct: Vec<&Session> = sessions
            .iter()
            .filter(|s| s.group == group)
            .collect();
        direct.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });

        if direct.is_empty() && node.children.is_empty() {
            rows.push(blank(group));
            return;
        }

        // Header always leads the group. When the folder has direct sessions,
        // use a sentinel id so the delete action stays hidden (id "" = empty).
        if direct.is_empty() {
            rows.push(blank(group));
        } else {
            rows.push(group_header_row(group, depth, label, collapsed));
        }

        if collapsed {
            return;
        }

        // Same-level child folders before this group's own sessions.
        for child in node.children.values() {
            emit_group_branch(sessions, child, rows, group_is_collapsed, blank);
        }

        for s in direct {
            rows.push(session_row(s, group, false, depth, label, false));
        }
    }

    for root in user_tree.values() {
        emit_group_branch(
            sessions,
            root,
            &mut rows,
            &group_is_collapsed,
            &blank,
        );
    }

    // Ungrouped sessions sit at the root of the tree (no folder header), below groups.
    let mut root_sessions: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.group.trim().is_empty() || is_reserved_session_group(s.group.trim()))
        .collect();
    root_sessions.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    for s in root_sessions {
        rows.push(session_row(s, "", false, 0, "", false));
    }

    model.set_vec(rows);
}

fn ancestor_collapsed(path: &str, group_is_collapsed: &impl Fn(&str) -> bool) -> bool {
    let mut rest = path;
    while let Some(idx) = rest.rfind('/') {
        rest = &rest[..idx];
        if group_is_collapsed(rest) {
            return true;
        }
    }
    false
}

/// Header row for a non-empty group. Sentinel id keeps Delete hidden and skips
/// SessionRow rendering in the welcome list.
fn group_header_row(group: &str, depth: i32, label: &str, collapsed: bool) -> SessionInfo {
    SessionInfo {
        id: "__group__".into(),
        name: "".into(),
        host: "".into(),
        port: 0,
        user: "".into(),
        auth: "".into(),
        last_used: "".into(),
        group: group.into(),
        group_header: group.into(),
        group_label: label.into(),
        group_depth: depth,
        collapsed,
        builtin: false,
        conn_kind: "".into(),
        endpoint: "".into(),
    }
}

fn session_row(
    s: &Session,
    group: &str,
    header: bool,
    depth: i32,
    label: &str,
    collapsed: bool,
) -> SessionInfo {
    SessionInfo {
        id: s.id.clone().into(),
        name: s.name.clone().into(),
        host: s.host.clone().into(),
        port: s.port as i32,
        user: s.user.clone().into(),
        auth: s.auth.as_str().into(),
        last_used: s
            .last_used
            .clone()
            .unwrap_or_else(|| "never".to_string())
            .into(),
        group: group.into(),
        group_header: if header { group.into() } else { "".into() },
        group_label: if header { label.into() } else { "".into() },
        group_depth: if header { depth } else { depth },
        collapsed,
        builtin: false,
        conn_kind: s.kind.as_str().into(),
        endpoint: session_endpoint(s).into(),
    }
}

// ---------------------------------------------------------------------------
// Session callbacks (welcome page + dialog)
// ---------------------------------------------------------------------------
