use crate::terminal::{cell_prefix, FindOptions};
use crate::ui::TermMatch;

fn build_find_regex(query: &str, opts: &FindOptions) -> Option<regex::Regex> {
    if query.is_empty() {
        return None;
    }
    let pattern = if opts.regex {
        if opts.whole_word {
            format!(r"(?:\b(?:{query})\b)")
        } else {
            query.to_string()
        }
    } else {
        let escaped = regex::escape(query);
        if opts.whole_word {
            format!(r"(?:\b(?:{escaped})\b)")
        } else {
            escaped
        }
    };
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!opts.case_sensitive)
        .build()
        .ok()
}

/// Find every occurrence of `query` across `rows` and return highlight
/// rectangles in GRID-COLUMN space (wide CJK glyphs count as two columns, so
/// highlights line up over the text #132). `TermMatch.row` is the index into
/// `rows` (absolute combined-buffer index when searching scrollback+live).
pub(crate) fn compute_find_matches(
    rows: &[String],
    query: &str,
    opts: &FindOptions,
) -> Vec<TermMatch> {
    let mut out: Vec<TermMatch> = Vec::new();
    if query.is_empty() {
        return out;
    }

    // Regex path (also used for whole-word + literal via escaped patterns).
    if opts.regex || opts.whole_word {
        let Some(re) = build_find_regex(query, opts) else {
            return out;
        };
        for (r, line) in rows.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            let prefix = cell_prefix(&chars);
            for mat in re.find_iter(line) {
                let start = line[..mat.start()].chars().count();
                let end = line[..mat.end()].chars().count();
                if start >= end || end > chars.len() {
                    continue;
                }
                out.push(TermMatch {
                    row: r as i32,
                    col: prefix[start] as i32,
                    len: (prefix[end] - prefix[start]) as i32,
                    current: false,
                });
            }
        }
        return out;
    }

    // Fast literal substring path (default: case-insensitive).
    let q: Vec<char> = if opts.case_sensitive {
        query.chars().collect()
    } else {
        query.chars().map(|c| c.to_ascii_lowercase()).collect()
    };
    if q.is_empty() {
        return out;
    }
    for (r, line) in rows.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let hay: Vec<char> = if opts.case_sensitive {
            chars.clone()
        } else {
            chars.iter().map(|c| c.to_ascii_lowercase()).collect()
        };
        let prefix = cell_prefix(&chars);
        let mut i = 0usize;
        while i + q.len() <= hay.len() {
            if hay[i..i + q.len()] == q[..] {
                let col = prefix[i] as i32;
                let len = (prefix[i + q.len()] - prefix[i]) as i32;
                out.push(TermMatch {
                    row: r as i32,
                    col,
                    len,
                    current: false,
                });
                i += q.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

