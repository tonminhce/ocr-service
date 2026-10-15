use chrono::{Datelike, NaiveDate};
use regex::Regex;
use std::sync::LazyLock;

static DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\b\d{1,2}[/-]\d{1,2}[/-]\d{4}\b|\b\d{4}-\d{2}-\d{2}\b)").unwrap()
});

pub(crate) fn date_span(value: &str) -> Option<(usize, usize, Option<String>)> {
    let m = DATE.find(value)?;
    let parsed = ["%d/%m/%Y", "%d-%m-%Y", "%Y-%m-%d"]
        .iter()
        .find_map(|f| NaiveDate::parse_from_str(m.as_str(), f).ok())
        .filter(|d| (1..=9999).contains(&d.year()))
        .map(|d| d.format("%Y-%m-%d").to_string());
    Some((m.start(), m.end(), parsed))
}

pub(crate) fn text_key(value: &str) -> String {
    value
        .to_lowercase()
        .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// No inferred magnitude/currency. Separators must be consistent, with 3-digit
/// grouping or an explicit 1-2 digit fractional component.
pub(crate) fn decimal(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return None;
    }
    if value.chars().all(|c| c.is_ascii_digit()) {
        return Some(trim_zeros(value));
    }
    let dots = value.matches('.').count();
    let commas = value.matches(',').count();
    let (group, fraction) = if dots > 0 && commas > 0 {
        let dot = value.rfind('.')?;
        let comma = value.rfind(',')?;
        let split = dot.max(comma);
        let fractional = &value[split + 1..];
        if fractional.is_empty() || fractional.len() > 2 {
            return None;
        }
        (&value[..split], Some(fractional))
    } else {
        let sep = if dots > 0 { '.' } else { ',' };
        let parts: Vec<_> = value.split(sep).collect();
        if parts.len() == 2 && (1..=2).contains(&parts[1].len()) {
            (parts[0], Some(parts[1]))
        } else {
            (value, None)
        }
    };
    let mut integer = String::new();
    if group.contains('.') || group.contains(',') {
        let separator = if group.contains('.') { '.' } else { ',' };
        if group.contains(if separator == '.' { ',' } else { '.' }) {
            return None;
        }
        let parts: Vec<_> = group.split(separator).collect();
        if !(1..=3).contains(&parts[0].len()) || parts.iter().skip(1).any(|p| p.len() != 3) {
            return None;
        }
        for p in parts {
            integer.push_str(p);
        }
    } else {
        integer.push_str(group);
    }
    if integer.is_empty() {
        return None;
    }
    let mut result = trim_zeros(&integer);
    if let Some(frac) = fraction {
        let frac = frac.trim_end_matches('0');
        if !frac.is_empty() {
            result.push('.');
            result.push_str(frac);
        }
    }
    Some(result)
}
fn trim_zeros(s: &str) -> String {
    let s = s.trim_start_matches('0');
    if s.is_empty() { "0".into() } else { s.into() }
}
