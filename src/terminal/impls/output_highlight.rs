use super::state::OutputHighlightPreset;
use crate::config::{normalize_highlight_color, OutputHighlightRule};
use crate::terminal::CompiledOutputRule;

pub(crate) fn compile_output_rules(rules: &[OutputHighlightRule]) -> Vec<CompiledOutputRule> {
    rules
        .iter()
        .filter(|rule| rule.enabled && !rule.pattern.trim().is_empty())
        .filter_map(|rule| {
            let pattern = if rule.regex {
                rule.pattern.clone()
            } else {
                regex::escape(&rule.pattern)
            };
            let matcher = regex::RegexBuilder::new(&pattern)
                .case_insensitive(!rule.case_sensitive)
                .build()
                .ok()?;
            Some(CompiledOutputRule {
                matcher,
                whole_line: rule.whole_line,
                fg: highlight_fg_color(&rule.color),
            })
        })
        .collect()
}

fn highlight_fg_color(color: &str) -> vt100::Color {
    let hex = normalize_highlight_color(color);
    let digits = hex.trim().trim_start_matches('#');
    if digits.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&digits[0..2], 16),
            u8::from_str_radix(&digits[2..4], 16),
            u8::from_str_radix(&digits[4..6], 16),
        ) {
            return vt100::Color::Rgb(r, g, b);
        }
    }
    vt100::Color::Rgb(0xf1, 0x4c, 0x4c)
}

impl OutputHighlightPreset {
    pub(crate) fn from_settings(enabled: bool, preset: &str) -> Self {
        if !enabled {
            Self::Off
        } else if preset == "devops" {
            Self::DevOps
        } else {
            Self::Log
        }
    }
}
