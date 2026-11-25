use std::collections::HashMap;
use std::ops::Range;

use serde::Serialize;

pub const MAX_UNITS: usize = 5_000_000;
const MAX_DOCUMENTS: usize = 256;
const MAX_TOTAL_UNITS: usize = 64_000_000;
const MAX_CHANGE_WORK: usize = 2_000_000_000;

pub const LF: u16 = 0x0A;
pub const CR: u16 = 0x0D;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug)]
pub struct Change<'a> {
    pub range: Option<(Position, Position)>,
    pub text: &'a str,
}

#[derive(Debug)]
pub struct Document {
    pub language: String,
    pub version: i64,
    pub units: Vec<u16>,
}

#[derive(Debug)]
pub struct Lines {
    starts: Vec<usize>,
    ends: Vec<usize>,
}

impl Lines {
    pub fn new(units: &[u16]) -> Self {
        let mut starts = vec![0];
        let mut ends = Vec::new();
        let mut index = 0;
        while index < units.len() {
            let unit = units[index];
            if unit == LF || unit == CR {
                ends.push(index);
                index += usize::from(unit == CR && units.get(index + 1) == Some(&LF)) + 1;
                starts.push(index);
            } else {
                index += 1;
            }
        }
        ends.push(units.len());
        Self { starts, ends }
    }

    pub fn offset(&self, units: &[u16], position: Position) -> Option<usize> {
        let line = position.line as usize;
        let Some(&start) = self.starts.get(line) else {
            return Some(units.len());
        };
        let offset = start.saturating_add(position.character as usize).min(self.ends[line]);
        let splits_pair = offset > 0
            && offset < units.len()
            && (0xD800..=0xDBFF).contains(&units[offset - 1])
            && (0xDC00..=0xDFFF).contains(&units[offset]);
        (!splits_pair).then_some(offset)
    }

    #[expect(clippy::cast_possible_truncation, reason = "documents are capped at MAX_UNITS")]
    pub fn position(&self, offset: usize) -> Position {
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        Position { line: line as u32, character: (offset.min(self.ends[line]) - self.starts[line]) as u32 }
    }

    pub fn span(&self, units: &[u16], range: (Position, Position)) -> Option<Range<usize>> {
        let start = self.offset(units, range.0)?;
        let end = self.offset(units, range.1)?;
        (start <= end).then_some(start..end)
    }
}

impl Document {
    pub fn span(&self, range: (Position, Position)) -> Option<Range<usize>> {
        Lines::new(&self.units).span(&self.units, range)
    }

    fn apply(&mut self, change: &Change<'_>) -> Option<()> {
        let text: Vec<u16> = change.text.encode_utf16().collect();
        let span = match change.range {
            None => 0..self.units.len(),
            Some(range) => self.span(range)?,
        };
        if self.units.len() - span.len() + text.len() > MAX_UNITS {
            return None;
        }
        self.units.splice(span, text);
        Some(())
    }
}

#[derive(Debug, Default)]
pub struct Documents {
    map: HashMap<String, Document>,
    total: usize,
}

impl Documents {
    pub fn get(&self, uri: &str) -> Option<&Document> {
        self.map.get(uri)
    }

    pub fn open(&mut self, uri: &str, language: &str, version: i64, text: &str) -> bool {
        self.close(uri);
        let count = text.encode_utf16().count();
        if count > MAX_UNITS || self.map.len() >= MAX_DOCUMENTS || self.total + count > MAX_TOTAL_UNITS {
            return false;
        }
        let mut units = Vec::with_capacity(count);
        units.extend(text.encode_utf16());
        self.total += count;
        let document = Document { language: language.to_owned(), version, units };
        self.map.insert(uri.to_owned(), document);
        true
    }

    pub fn close(&mut self, uri: &str) {
        if let Some(document) = self.map.remove(uri) {
            self.total -= document.units.len();
        }
    }

    pub fn change(&mut self, uri: &str, version: i64, changes: &[Change<'_>]) -> bool {
        let Self { map, total } = self;
        let Some(document) = map.get_mut(uri) else {
            return false;
        };
        let before = document.units.len();
        let mut work = 0usize;
        let applied = version > document.version
            && changes.iter().all(|change| {
                work = work.saturating_add(document.units.len());
                work <= MAX_CHANGE_WORK && document.apply(change).is_some()
            });
        *total = *total - before + document.units.len();
        if !applied || *total > MAX_TOTAL_UNITS {
            self.close(uri);
            return false;
        }
        document.version = version;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn indexes_every_line_ending() {
        let text = units("a\nbb\r\nc\rd");
        let lines = Lines::new(&text);
        assert_eq!(lines.offset(&text, at(1, 1)), Some(3));
        assert_eq!(lines.offset(&text, at(2, 0)), Some(6));
        assert_eq!(lines.offset(&text, at(3, 1)), Some(9));
        assert_eq!(lines.position(3), at(1, 1));
        assert_eq!(lines.position(8), at(3, 0));
        assert_eq!(lines.position(9), at(3, 1));
    }

    #[test]
    fn clamps_positions_past_line_or_document_end() {
        let text = units("ab\r\ncd");
        let lines = Lines::new(&text);
        assert_eq!(lines.offset(&text, at(0, 99)), Some(2));
        assert_eq!(lines.offset(&text, at(9, 0)), Some(text.len()));
    }

    #[test]
    fn counts_utf16_and_refuses_to_split_a_pair() {
        let text = units("😀ы");
        let lines = Lines::new(&text);
        assert_eq!(lines.offset(&text, at(0, 2)), Some(2));
        assert_eq!(lines.offset(&text, at(0, 1)), None);
    }

    #[test]
    fn refuses_reversed_range() {
        let text = units("abc");
        let lines = Lines::new(&text);
        assert_eq!(lines.span(&text, (at(0, 2), at(0, 1))), None);
    }

    #[test]
    fn applies_incremental_changes_in_order() {
        let mut documents = Documents::default();
        assert!(documents.open("a", "markdown", 1, "Корвин\nРэндом"));
        let changes = [
            Change { range: Some((at(1, 0), at(1, 6))), text: "Оберон" },
            Change { range: Some((at(0, 6), at(0, 6))), text: " и 😀" },
        ];
        assert!(documents.change("a", 2, &changes));
        let document = documents.get("a").unwrap();
        assert_eq!(String::from_utf16(&document.units).unwrap(), "Корвин и 😀\nОберон");
        assert_eq!(document.version, 2);
    }

    #[test]
    fn replaces_whole_text_without_range() {
        let mut documents = Documents::default();
        documents.open("a", "markdown", 1, "старое");
        let change = Change { range: None, text: "новое" };
        assert!(documents.change("a", 2, &[change]));
        assert_eq!(documents.get("a").unwrap().units, units("новое"));
    }

    #[test]
    fn drops_document_on_invalid_change() {
        let mut documents = Documents::default();
        documents.open("a", "markdown", 5, "😀");
        let split = Change { range: Some((at(0, 1), at(0, 1))), text: "x" };
        assert!(!documents.change("a", 6, &[split]));
        assert!(documents.get("a").is_none());
        assert_eq!(documents.total, 0);
    }

    #[test]
    fn drops_document_on_stale_version() {
        let mut documents = Documents::default();
        documents.open("a", "markdown", 5, "текст");
        let change = Change { range: None, text: "x" };
        assert!(!documents.change("a", 5, &[change]));
        assert!(documents.get("a").is_none());
    }

    #[test]
    fn enforces_size_ceilings() {
        let mut documents = Documents::default();
        assert!(!documents.open("big", "plaintext", 1, &"a".repeat(MAX_UNITS + 1)));
        assert!(documents.open("a", "plaintext", 1, &"a".repeat(MAX_UNITS)));
        let grow = Change { range: Some((at(0, 0), at(0, 0))), text: "b" };
        assert!(!documents.change("a", 2, &[grow]));
        assert!(documents.get("a").is_none());
        for index in 0..MAX_DOCUMENTS {
            assert!(documents.open(&index.to_string(), "plaintext", 1, ""));
        }
        assert!(!documents.open("extra", "plaintext", 1, ""));
    }

    #[test]
    fn tracks_total_size_across_changes_and_close() {
        let mut documents = Documents::default();
        documents.open("a", "plaintext", 1, "abc");
        documents.open("b", "plaintext", 1, "de");
        let change = Change { range: None, text: "z" };
        documents.change("a", 2, &[change]);
        assert_eq!(documents.total, 3);
        documents.close("b");
        assert_eq!(documents.total, 1);
    }
}
