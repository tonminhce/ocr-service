//! The course's GLM-OCR SDK 0.1.4 layout formatter, implemented in Rust.
//!
//! The label mapping and formatting order are fixed to the course configuration.
//! Hyphenation uses the SDK's dependency-free fallback rather than `wordfreq`.
//! Repetition detection has a fixed work budget: when exhausted, the original
//! content is preserved (apart from the SDK's cheaper line repetition cleanup).

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutResult {
    pub index: u32,
    pub label: String,
    pub native_label: String,
    pub content: Option<String>,
    pub raw_content: Option<String>,
    pub bbox_2d: [i32; 4],
    pub polygon: Vec<[i32; 2]>,
    pub score: f32,
    pub reading_order: u32,
    pub task_type: String,
    pub polygon_source: String,
}

/// Format regions sorted by their detector reading-order index.
///
/// The JSON-compatible regions preserve audit metadata in addition to the SDK
/// fields. Markdown image page indices are zero-based, as in the pinned SDK.
pub fn format_pages(grouped: Vec<Vec<LayoutResult>>) -> (Vec<Vec<LayoutResult>>, String) {
    let mut pages = Vec::with_capacity(grouped.len());
    for mut page in grouped {
        page.sort_by_key(|region| region.index);
        let mut formatted = Vec::with_capacity(page.len());
        for mut region in page {
            region.native_label = region.label.clone();
            if region.raw_content.is_none() {
                region.raw_content = region.content.clone();
            }
            region.label = map_label(&region.native_label).to_owned();
            region.content = region
                .content
                .as_deref()
                .map(|value| format_content(value, &region.label, &region.native_label));
            if region.label != "image"
                && region
                    .content
                    .as_deref()
                    .is_some_and(|s| py_trim(s).is_empty())
            {
                continue;
            }
            region.index = formatted.len() as u32;
            formatted.push(region);
        }
        let mut formatted = merge_text_blocks(merge_formula_numbers(formatted));
        format_bullet_points(&mut formatted);
        pages.push(formatted);
    }

    let markdown = pages
        .iter()
        .enumerate()
        .map(|(page_index, page)| {
            page.iter()
                .filter_map(|region| {
                    if region.label == "image" {
                        // Rust's array Debug format matches Python's integer list.
                        Some(format!("![](page={page_index},bbox={:?})", region.bbox_2d))
                    } else {
                        region.content.as_ref().filter(|s| !s.is_empty()).cloned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    (pages, markdown)
}

fn map_label(native_label: &str) -> &str {
    match native_label {
        "chart" | "image" => "image",
        "abstract" | "algorithm" | "content" | "doc_title" | "figure_title" | "paragraph_title"
        | "reference_content" | "text" | "vertical_text" | "vision_footnote" | "seal"
        | "formula_number" => "text",
        "table" => "table",
        "display_formula" | "inline_formula" => "formula",
        other => other,
    }
}

// Python str.strip/split treat these four information separators as whitespace,
// in addition to Unicode White_Space, unlike Rust char::is_whitespace.
fn py_whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_trim(s: &str) -> &str {
    s.trim_matches(py_whitespace)
}

fn py_trim_start(s: &str) -> &str {
    s.trim_start_matches(py_whitespace)
}

fn py_trim_end(s: &str) -> &str {
    s.trim_end_matches(py_whitespace)
}

fn clean_content(content: &str) -> String {
    // Preserve the SDK's order: literal \\t removal precedes whitespace removal.
    let content = content.trim_start_matches("\\t");
    let content = py_trim_start(content);
    // Python's `$` also matches immediately before one final LF. Since rstrip
    // runs next, removing that LF here reproduces the same suffix behavior.
    let content = content.strip_suffix('\n').unwrap_or(content);
    let content = content.trim_end_matches("\\t");
    let content = py_trim_end(content);
    let content = collapse_char_runs(content);
    let mut content = collapse_escaped_underscores(&content);
    if content.chars().count() >= 2048 {
        content = clean_repeated_content(&content);
    }
    py_trim(&content).to_owned()
}

fn collapse_char_runs(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut previous = None;
    let mut run_length = 0usize;
    for c in content.chars() {
        if previous == Some(c) {
            run_length += 1;
        } else {
            previous = Some(c);
            run_length = 1;
        }
        if !matches!(c, '.' | '·' | '_') || run_length <= 3 {
            result.push(c);
        }
    }
    result
}

fn collapse_escaped_underscores(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut remaining = content;
    while let Some(start) = remaining.find("\\_") {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let mut count = 0usize;
        while let Some(rest) = remaining.strip_prefix("\\_") {
            remaining = rest;
            count += 1;
        }
        for _ in 0..count.min(3) {
            result.push_str("\\_");
        }
    }
    result.push_str(remaining);
    result
}

fn strip_heading_marks(content: &str) -> &str {
    if content.starts_with('#') {
        py_trim_start(content.trim_start_matches('#'))
    } else {
        content
    }
}

fn parenthesized_list_marker() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^[（(](\d+|[A-Za-z])[）)]([^\n]*)$").unwrap())
}

fn ordered_list_marker() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^(\d+|[A-Za-z])([.)）])([^\n]*)$").unwrap())
}

fn format_content(content: &str, label: &str, native_label: &str) -> String {
    let mut content = clean_content(content);
    if native_label == "doc_title" {
        content = format!("# {}", strip_heading_marks(&content));
    } else if native_label == "paragraph_title" {
        let body = content
            .strip_prefix("- ")
            .or_else(|| content.strip_prefix("* "))
            .map_or(content.as_str(), py_trim_start);
        content = format!("## {}", py_trim_start(strip_heading_marks(body)));
    }

    if label == "formula" {
        let body = [("$$", "$$"), ("\\[", "\\]"), ("\\(", "\\)")]
            .into_iter()
            .find_map(|(prefix, suffix)| {
                // The SDK also accepts overlapping delimiters, e.g. "$$".
                if content.starts_with(prefix) && content.ends_with(suffix) {
                    Some(if content.len() < prefix.len() + suffix.len() {
                        ""
                    } else {
                        &content[prefix.len()..content.len() - suffix.len()]
                    })
                } else {
                    None
                }
            });
        content = format!("$$\n{}\n$$", body.map_or(content.as_str(), py_trim));
    }

    if label == "text" {
        if content.starts_with('·') || content.starts_with('•') || content.starts_with("* ") {
            let rest = &content[content.chars().next().unwrap().len_utf8()..];
            content = format!("- {}", py_trim_start(rest));
        }
        if let Some(marker) = parenthesized_list_marker().captures(&content) {
            content = format!("({}) {}", &marker[1], py_trim_start(&marker[2]));
        }
        if let Some(marker) = ordered_list_marker().captures(&content) {
            let separator = if &marker[2] == "）" { ")" } else { &marker[2] };
            content = format!("{}{separator} {}", &marker[1], py_trim_start(&marker[3]));
        }
        content = double_single_newlines(&content);
    }
    content
}

fn double_single_newlines(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut previous = None;
    while let Some(c) = chars.next() {
        result.push(c);
        if c == '\n' && previous != Some('\n') && chars.peek() != Some(&'\n') {
            result.push('\n');
        }
        previous = Some(c);
    }
    result
}

fn clean_formula_number(content: &str) -> &str {
    let content = py_trim(content);
    content
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .or_else(|| {
            content
                .strip_prefix('（')
                .and_then(|value| value.strip_suffix('）'))
        })
        .unwrap_or(content)
}

fn add_formula_tag(formula: &mut LayoutResult, number: Option<&str>) {
    if let Some(content) = formula
        .content
        .as_ref()
        .and_then(|s| s.strip_suffix("\n$$"))
    {
        formula.content = Some(format!(
            "{content} \\tag{{{}}}\n$$",
            clean_formula_number(number.unwrap_or(""))
        ));
    }
}

fn merge_formula_numbers(page: Vec<LayoutResult>) -> Vec<LayoutResult> {
    let mut merged = Vec::with_capacity(page.len());
    let mut index = 0usize;
    while index < page.len() {
        let region = &page[index];
        let next = page.get(index + 1);
        if region.native_label == "formula_number" {
            if let Some(next) = next.filter(|value| value.label == "formula") {
                let mut formula = next.clone();
                add_formula_tag(&mut formula, region.content.as_deref());
                merged.push(formula);
                index += 2;
                continue;
            }
            // Standalone formula numbers are dropped by the SDK.
        } else if let Some(next) =
            next.filter(|value| region.label == "formula" && value.native_label == "formula_number")
        {
            let mut formula = region.clone();
            add_formula_tag(&mut formula, next.content.as_deref());
            merged.push(formula);
            index += 2;
            continue;
        } else {
            merged.push(region.clone());
        }
        index += 1;
    }
    reindex(&mut merged);
    merged
}

fn likely_valid_merged_word(merged_word: &str) -> bool {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    let token = py_trim(merged_word).to_lowercase();
    REGEX
        .get_or_init(|| Regex::new(r"^[a-z][a-z'\-]{2,30}$").unwrap())
        .is_match(&token)
        && !token.contains("--")
        && !token.contains("''")
}

fn merge_text_blocks(page: Vec<LayoutResult>) -> Vec<LayoutResult> {
    let mut merged = Vec::with_capacity(page.len());
    let mut skipped = vec![false; page.len()];
    for (index, region) in page.iter().enumerate() {
        if skipped[index] {
            continue;
        }
        let mut result = region.clone();
        if let Some(before) = region
            .content
            .as_deref()
            .filter(|_| region.label == "text")
            .map(py_trim_end)
            .and_then(|s| s.strip_suffix('-'))
        {
            for (next_index, next) in page.iter().enumerate().skip(index + 1) {
                if next.label != "text" {
                    continue;
                }
                let Some(after) = next.content.as_deref().map(py_trim_start) else {
                    continue;
                };
                if !after.chars().next().is_some_and(char::is_lowercase) {
                    continue;
                }
                let before_word = before.split(py_whitespace).rfind(|s| !s.is_empty());
                let after_word = after.split(py_whitespace).find(|s| !s.is_empty());
                if let (Some(before_word), Some(after_word)) = (before_word, after_word)
                    && likely_valid_merged_word(&format!("{before_word}{after_word}"))
                {
                    result.content = Some(format!("{before}{after}"));
                    skipped[next_index] = true;
                }
                // Match the SDK: the first lowercase text block ends the search,
                // even when the fallback heuristic rejects the combined token.
                break;
            }
        }
        merged.push(result);
    }
    reindex(&mut merged);
    merged
}

fn reindex(page: &mut [LayoutResult]) {
    for (index, region) in page.iter_mut().enumerate() {
        region.index = index as u32;
    }
}

fn format_bullet_points(page: &mut [LayoutResult]) {
    for index in 1..page.len().saturating_sub(1) {
        let (prefix, rest) = page.split_at_mut(index);
        let previous = &prefix[index - 1];
        let (current, suffix) = rest.split_first_mut().unwrap();
        let next = &suffix[0];
        if current.native_label != "text"
            || previous.native_label != "text"
            || next.native_label != "text"
        {
            continue;
        }
        let Some(content) = current.content.as_ref() else {
            continue;
        };
        if content.starts_with("- ")
            || !previous
                .content
                .as_deref()
                .is_some_and(|s| s.starts_with("- "))
            || !next.content.as_deref().is_some_and(|s| s.starts_with("- "))
        {
            continue;
        }
        // Widen before subtraction so malformed extreme coordinates cannot panic.
        let left = i64::from(current.bbox_2d[0]);
        if (left - i64::from(previous.bbox_2d[0])).abs() <= 10
            && (left - i64::from(next.bbox_2d[0])).abs() <= 10
        {
            current.content = Some(format!("- {content}"));
        }
    }
}

const MAX_REPEAT_SCAN_CHARS: usize = 65_536;
const MAX_REPEAT_CANDIDATES: usize = 2_000_000;

fn clean_repeated_content(content: &str) -> String {
    let stripped = py_trim(content);
    if stripped.is_empty() {
        return content.to_owned();
    }
    if let Some(clean) = find_consecutive_repeat(stripped) {
        return clean;
    }

    let lines: Vec<_> = content
        .split('\n')
        .map(py_trim)
        .filter(|s| !s.is_empty())
        .collect();
    if lines.len() < 10 {
        return content.to_owned();
    }
    let mut frequencies: HashMap<&str, usize> = HashMap::new();
    // Python Counter ties favor the first line encountered.
    let mut common = "";
    let mut highest_count = 0usize;
    for line in &lines {
        *frequencies.entry(line).or_default() += 1;
    }
    for line in &lines {
        let count = frequencies[line];
        if count > highest_count {
            common = line;
            highest_count = count;
        }
    }
    if highest_count < 10 || highest_count as f64 / (lines.len() as f64) < 0.8 {
        return content.to_owned();
    }
    if let Some(start) = lines
        .windows(3)
        .position(|window| window.iter().all(|s| *s == common))
    {
        let mut nonempty = 0usize;
        for (index, line) in content.split('\n').enumerate() {
            if !py_trim(line).is_empty() {
                nonempty += 1;
                if nonempty == start + 1 {
                    return content
                        .split('\n')
                        .take(index + 1)
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
        }
    }
    content.to_owned()
}

/// The SDK regex searches the earliest starting position and then the shortest
/// unit of at least 10 code points repeated at least 10 times. It discards the
/// entire tail after that first unit, including otherwise nonrepeating text.
fn find_consecutive_repeat(content: &str) -> Option<String> {
    let chars: Vec<char> = content.chars().take(MAX_REPEAT_SCAN_CHARS + 1).collect();
    let length = chars.len();
    if !(100..=MAX_REPEAT_SCAN_CHARS).contains(&length) {
        return None;
    }
    // Rolling hashes cheaply reject candidates; equal hashes are always checked
    // against the real characters, so collisions cannot remove valid content.
    let mut hashes = vec![0u64; length + 1];
    let mut powers = vec![1u64; length + 1];
    for (index, c) in chars.iter().enumerate() {
        hashes[index + 1] = hashes[index]
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(*c as u32) + 1);
        powers[index + 1] = powers[index].wrapping_mul(1_000_003);
    }
    let hash = |start: usize, len: usize| {
        hashes[start + len].wrapping_sub(hashes[start].wrapping_mul(powers[len]))
    };
    let mut candidates = 0usize;
    for start in 0..=length - 100 {
        for unit_len in 10..=(length - start) / 10 {
            candidates += 1;
            if candidates > MAX_REPEAT_CANDIDATES {
                return None;
            }
            let target = hash(start, unit_len);
            if (1..10).all(|repeat| hash(start + repeat * unit_len, unit_len) == target) {
                let unit = &chars[start..start + unit_len];
                if (1..10).all(|repeat| {
                    let repeated = start + repeat * unit_len;
                    &chars[repeated..repeated + unit_len] == unit
                }) {
                    return Some(chars[..start + unit_len].iter().collect());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn region(index: u32, label: &str, content: Option<&str>, left: i32) -> LayoutResult {
        LayoutResult {
            index,
            label: label.into(),
            native_label: label.into(),
            content: content.map(str::to_owned),
            raw_content: None,
            bbox_2d: [left, 20, 600, 80],
            polygon: vec![[left, 20], [600, 20], [600, 80], [left, 80]],
            score: 0.9,
            reading_order: index,
            task_type: if label == "image" || label == "chart" {
                "skip"
            } else {
                "text"
            }
            .into(),
            polygon_source: "detector".into(),
        }
    }

    fn core_json(pages: &[Vec<LayoutResult>]) -> Value {
        Value::Array(
            pages
                .iter()
                .map(|page| {
                    Value::Array(
                        page.iter()
                            .map(|region| {
                                json!({
                                    "index": region.index,
                                    "label": region.label,
                                    "native_label": region.native_label,
                                    "content": region.content,
                                    "bbox_2d": region.bbox_2d,
                                    "polygon": region.polygon,
                                })
                            })
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn pinned_sdk_014_golden_layout_json_and_markdown() {
        // Golden output was generated by the repository's pinned wheel source,
        // executing ResultFormatter.process with wordfreq disabled and the
        // production-ocr-course/client_rt_consumer/config.yaml label mapping.
        let input = vec![
            vec![
                region(9, "text", Some("mation is preserved"), 200),
                region(0, "doc_title", Some("### Tài liệu – OCR"), 100),
                region(2, "formula_number", Some("（１２）"), 100),
                region(3, "display_formula", Some(r"\[ α + β = γ \]"), 100),
                region(4, "text", Some("• mục một"), 100),
                region(5, "text", Some("Tiếng Việt\nthứ hai"), 105),
                region(6, "text", Some("* mục ba"), 101),
                region(7, "text", Some("infor-"), 200),
                region(8, "chart", None, 400),
                region(10, "text", Some("   "), 100),
                region(11, "formula_number", Some("(99)"), 100),
            ],
            vec![
                region(0, "paragraph_title", Some("* ### Phần hai"), 100),
                region(1, "text", Some("(a)xin chào"), 100),
                region(2, "table", Some("| A | B |\n|---|---|\n| 1 | 2 |"), 100),
                region(3, "image", None, 250),
                region(4, "text", Some(r"\t   ..... ····· ____ \_\_\_\_  \t"), 100),
                region(5, "text", Some("（١٢）قائمة"), 100),
                region(6, "inline_formula", Some("$$ E = mc² $$"), 100),
                region(7, "formula_number", Some("(3.1)"), 100),
                region(8, "text", Some("1.one\n2.two"), 100),
            ],
            vec![],
            vec![region(0, "text", None, 100)],
        ];
        let (pages, markdown) = format_pages(input);
        let expected: Value = serde_json::from_str(r###"[
          [
            {"index":0,"label":"text","content":"# Tài liệu – OCR","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"doc_title"},
            {"index":1,"label":"formula","content":"$$\nα + β = γ \\tag{１２}\n$$","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"display_formula"},
            {"index":2,"label":"text","content":"- mục một","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"text"},
            {"index":3,"label":"text","content":"- Tiếng Việt\n\nthứ hai","bbox_2d":[105,20,600,80],"polygon":[[105,20],[600,20],[600,80],[105,80]],"native_label":"text"},
            {"index":4,"label":"text","content":"- mục ba","bbox_2d":[101,20,600,80],"polygon":[[101,20],[600,20],[600,80],[101,80]],"native_label":"text"},
            {"index":5,"label":"text","content":"information is preserved","bbox_2d":[200,20,600,80],"polygon":[[200,20],[600,20],[600,80],[200,80]],"native_label":"text"},
            {"index":6,"label":"image","content":null,"bbox_2d":[400,20,600,80],"polygon":[[400,20],[600,20],[600,80],[400,80]],"native_label":"chart"}
          ],
          [
            {"index":0,"label":"text","content":"## Phần hai","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"paragraph_title"},
            {"index":1,"label":"text","content":"(a) xin chào","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"text"},
            {"index":2,"label":"table","content":"| A | B |\n|---|---|\n| 1 | 2 |","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"table"},
            {"index":3,"label":"image","content":null,"bbox_2d":[250,20,600,80],"polygon":[[250,20],[600,20],[600,80],[250,80]],"native_label":"image"},
            {"index":4,"label":"text","content":"... ··· ___ \\_\\_\\_","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"text"},
            {"index":5,"label":"text","content":"(١٢) قائمة","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"text"},
            {"index":6,"label":"formula","content":"$$\nE = mc² \\tag{3.1}\n$$","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"inline_formula"},
            {"index":7,"label":"text","content":"1.one\n\n2.two","bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"text"}
          ],
          [],
          [{"index":0,"label":"text","content":null,"bbox_2d":[100,20,600,80],"polygon":[[100,20],[600,20],[600,80],[100,80]],"native_label":"text"}]
        ]"###).unwrap();
        assert_eq!(core_json(&pages), expected);
        assert_eq!(
            markdown,
            concat!(
                "# Tài liệu – OCR\n\n$$\nα + β = γ \\tag{１２}\n$$\n\n",
                "- mục một\n\n- Tiếng Việt\n\nthứ hai\n\n- mục ba\n\n",
                "information is preserved\n\n![](page=0,bbox=[400, 20, 600, 80])\n\n",
                "## Phần hai\n\n(a) xin chào\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n",
                "![](page=1,bbox=[250, 20, 600, 80])\n\n... ··· ___ \\_\\_\\_\n\n",
                "(١٢) قائمة\n\n$$\nE = mc² \\tag{3.1}\n$$\n\n1.one\n\n2.two\n\n\n\n",
            )
        );
        assert_eq!(
            pages[0][0].raw_content.as_deref(),
            Some("### Tài liệu – OCR")
        );
        assert_eq!(pages[0][5].raw_content.as_deref(), Some("infor-"));
        assert_eq!(pages[0][5].reading_order, 7);
        assert_eq!(pages[0][6].task_type, "skip");
        assert_eq!(pages[0][6].score, 0.9);
    }

    #[test]
    fn pinned_sdk_cleanup_and_repeated_content_golden() {
        assert_eq!(clean_content("\u{1c}  Xin chào\u{1f}"), "Xin chào");
        assert_eq!(clean_content(r"\t word\t   "), r"word\t");
        assert_eq!(clean_content(r"  \tword\t"), r"\tword");
        assert_eq!(clean_content("word\\t\n"), "word");
        assert_eq!(clean_content("word\\t\n\n"), r"word\t");
        assert_eq!(clean_content("word\\t\r\n"), r"word\t");
        assert_eq!(
            clean_content(&format!("{}suffix", "abcdefghij".repeat(210))),
            "abcdefghij"
        );
        assert_eq!(
            clean_content(&format!("{}after", "x".repeat(2048))),
            "xxxxxxxxxx"
        );
        assert_eq!(
            clean_content(&format!("{}suffix", "Tiếng Việt ăăăăăăăăă".repeat(110))),
            "Tiếng Việt ăăăăăăăăă"
        );
        assert_eq!(
            clean_content(&format!(
                "long unique prefix\n{}TAIL",
                "Đây là một dòng rất dài cho kiểm tra.\n".repeat(70)
            )),
            "long unique prefix\nĐây là một dòng rất dài cho kiểm tra."
        );
        // This input stays below the consecutive repetition unit size, so the
        // utility's line-level repetition fallback handles it instead.
        assert_eq!(
            clean_repeated_content("lead\nx\nx\nx\nx\nx\nx\nx\nx\nx\nx\nx\nx\ntail"),
            "lead\nx"
        );
    }

    #[test]
    fn single_newlines_and_unicode_markers_match_sdk() {
        assert_eq!(
            format_content("A\nB\n\nC\n\n\nD", "text", "text"),
            "A\n\nB\n\nC\n\n\nD"
        );
        assert_eq!(
            format_content("１２） Việt Nam", "text", "text"),
            "１２) Việt Nam"
        );
        assert_eq!(format_content("（b)   mục", "text", "text"), "(b) mục");
        assert_eq!(format_content("AB.item", "text", "text"), "AB.item");
        assert_eq!(
            format_content("* tiếng Việt", "text", "text"),
            "- tiếng Việt"
        );
        assert_eq!(
            format_content("1.first\n2.second", "text", "text"),
            "1.first\n\n2.second"
        );
        assert_eq!(format_content("\r\nX", "text", "text"), "X");
    }

    #[test]
    fn hyphen_merging_uses_sdk_fallback_and_search_order() {
        let (pages, _) = format_pages(vec![vec![
            region(0, "text", Some("inter-"), 100),
            region(1, "text", Some("UPPER"), 100),
            region(2, "text", Some("national cooperation"), 100),
            region(3, "text", Some("Việt-"), 100),
            region(4, "text", Some("nam đẹp"), 100),
            region(5, "text", Some("frag-"), 100),
            region(6, "text", Some("ments."), 100),
        ]]);
        let content: Vec<_> = pages[0]
            .iter()
            .map(|s| s.content.as_deref().unwrap())
            .collect();
        assert_eq!(
            content,
            [
                "international cooperation",
                "UPPER",
                "Việt-",
                "nam đẹp",
                "frag-",
                "ments."
            ]
        );
        assert!(likely_valid_merged_word("information"));
        assert!(likely_valid_merged_word("abc'd"));
        assert!(!likely_valid_merged_word("two--words"));
        assert!(!likely_valid_merged_word("TiếngViệt"));
    }

    #[test]
    fn failed_recognition_content_and_extreme_boxes_are_safe() {
        // SDK 0.1.4 raises AttributeError for some null text/number combinations.
        // Preserve failed text regions, and allow nullable numbers without panic.
        let (pages, markdown) = format_pages(vec![vec![
            region(0, "text", Some("• first"), i32::MIN),
            region(1, "text", None, i32::MAX),
            region(2, "text", Some("• last"), i32::MIN),
            region(3, "formula_number", None, 100),
            region(4, "display_formula", Some("α"), 100),
        ]]);
        assert_eq!(pages[0].len(), 4);
        assert_eq!(pages[0][1].content, None);
        assert!(markdown.contains("$$\nα \\tag{}\n$$"));
        let (pages, _) = format_pages(vec![vec![
            region(0, "text", Some("• first"), i32::MIN),
            region(1, "text", Some("middle"), i32::MAX),
            region(2, "text", Some("• last"), i32::MIN),
        ]]);
        assert_eq!(pages[0][1].content.as_deref(), Some("middle"));
    }

    #[test]
    fn formula_wrappers_are_unicode_safe_even_when_empty() {
        for content in ["$$", "$$$$", r"\[\]", r"\(\)"] {
            assert_eq!(
                format_content(content, "formula", "display_formula"),
                "$$\n\n$$"
            );
        }
        assert_eq!(
            format_content(r"\( λ + π \)", "formula", "inline_formula"),
            "$$\nλ + π\n$$"
        );
        assert_eq!(clean_formula_number("（２.１）"), "２.１");
        assert_eq!(clean_formula_number("（２.１)"), "（２.１)");
    }

    #[test]
    fn repetition_detection_is_bounded_and_preserves_unscanned_content() {
        let content = "abcdefghij".repeat(7000);
        assert_eq!(clean_content(&content), content);
        let content = "Việt Nam rất đẹp ".repeat(5000);
        assert_eq!(clean_content(&content), content.trim());
    }

    #[test]
    fn bullet_repair_respects_native_label_and_alignment_threshold() {
        for (label, left, expected) in [
            ("text", 110, "- middle"),
            ("text", 111, "middle"),
            ("content", 100, "middle"),
        ] {
            let (pages, _) = format_pages(vec![vec![
                region(0, "text", Some("• first"), 100),
                region(1, label, Some("middle"), left),
                region(2, "text", Some("• last"), 100),
            ]]);
            assert_eq!(pages[0][1].content.as_deref(), Some(expected));
        }
    }
}
