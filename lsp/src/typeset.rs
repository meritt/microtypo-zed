use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;
use std::sync::LazyLock;
use std::thread::{self, ScopedJoinHandle};
use std::time::Instant;

use microtypo_core::{ErrorKind, Host, Input, MicroTypo, ProcessOptions};

use crate::config::Settings;
use crate::document::{CR, LF};

const ENGINE_STACK: usize = 8 * 1024 * 1024;

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

const HOST: Host = Host {
    now_ms: || START.elapsed().as_secs_f64() * 1000.0,
    fill_random: |bytes| getrandom::fill(bytes).expect("the system CSPRNG is available"),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Config,
    Input,
    Budget,
    Internal,
}

impl Failure {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Input => "input",
            Self::Budget => "budget",
            Self::Internal => "internal",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Input => "MicroTypo: the document is malformed or too long",
            Self::Budget => "MicroTypo: the document took too long to process",
            Self::Config | Self::Internal => "MicroTypo: typesetting failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replace {
    pub span: Range<usize>,
    pub text: Vec<u16>,
}

pub fn is_space(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(char::is_whitespace)
}

fn core(units: &[u16]) -> Range<usize> {
    let text = trimmed(units, 0..units.len());
    let start =
        units[..text.start].iter().rposition(|&unit| unit == LF || unit == CR).map_or(0, |line_end| line_end + 1);
    start..text.end
}

pub fn trimmed(units: &[u16], span: Range<usize>) -> Range<usize> {
    let slice = &units[span.clone()];
    let Some(first) = slice.iter().position(|&unit| !is_space(unit)) else {
        return span.start..span.start;
    };
    let last = slice.iter().rposition(|&unit| !is_space(unit)).unwrap_or(first);
    span.start + first..span.start + last + 1
}

fn runs(units: &[u16]) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut start = 0;
    for index in 1..=units.len() {
        if index == units.len() || is_space(units[index]) != is_space(units[start]) {
            result.push(start..index);
            start = index;
        }
    }
    result
}

fn with_endings(old: &[u16], new: &[u16]) -> Vec<u16> {
    let crlf = old.windows(2).any(|pair| pair == [CR, LF]);
    let mut result = Vec::with_capacity(new.len() + 1);
    let mut previous = 0;
    for &unit in new {
        if crlf && unit == LF && previous != CR {
            result.push(CR);
        }
        result.push(unit);
        previous = unit;
    }
    result
}

fn refine(units: &[u16], span: Range<usize>, text: &[u16]) -> Vec<Replace> {
    let old = &units[span.clone()];
    let (old_runs, new_runs) = (runs(old), runs(text));
    let aligned = old_runs.len() == new_runs.len()
        && !old.is_empty()
        && !text.is_empty()
        && is_space(old[0]) == is_space(text[0]);
    let pairs =
        if aligned { old_runs.into_iter().zip(new_runs).collect() } else { vec![(0..old.len(), 0..text.len())] };
    pairs
        .into_iter()
        .filter_map(|(from, to)| {
            let before = &old[from.clone()];
            let mut after = with_endings(before, &text[to]);
            let splits_crlf = before.last() == Some(&CR) && units.get(span.start + from.end) == Some(&LF);
            let end = from.end - usize::from(splits_crlf);
            if splits_crlf && after.last() == Some(&CR) {
                after.pop();
            }
            (old[from.start..end] != *after)
                .then(|| Replace { span: span.start + from.start..span.start + end, text: after })
        })
        .collect()
}

#[derive(Debug)]
pub struct Typesetter {
    settings: Settings,
    engines: HashMap<&'static str, MicroTypo>,
}

impl Typesetter {
    pub fn new(settings: Settings) -> Result<Self, Failure> {
        let mut typesetter = Self { settings, engines: HashMap::new() };
        typesetter.engine("plain")?;
        Ok(typesetter)
    }

    pub fn formats(&self, language: &str) -> bool {
        self.settings.format.contains(language)
    }

    pub fn registered(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.settings.format.iter().copied()
    }

    fn engine(&mut self, format: &'static str) -> Result<&mut MicroTypo, Failure> {
        match self.engines.entry(format) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let mut config = self.settings.engine.clone();
                config.input = Some(Input { format: Some(format.to_owned()), ..Input::default() });
                let engine = MicroTypo::with_config(config, HOST).map_err(|_| Failure::Config)?;
                Ok(entry.insert(engine))
            }
        }
    }

    pub fn edits(
        &mut self,
        units: &[u16],
        format: &'static str,
        scope: Option<Range<usize>>,
    ) -> Result<Vec<Replace>, Failure> {
        let body = core(units);
        let scope = scope.map_or_else(|| body.clone(), |span| trimmed(units, span));
        let scope = scope.start.max(body.start)..scope.end.min(body.end);
        if scope.start >= scope.end {
            return Ok(Vec::new());
        }

        let options = ProcessOptions { edits: true, ..ProcessOptions::default() };
        let input = &units[body.clone()];
        let outcome = thread::scope(|scope| {
            let worker = thread::Builder::new().stack_size(ENGINE_STACK).spawn_scoped(scope, || {
                let engine = self.engine(format)?;
                engine.process_with_diagnostics(input, options).map_err(|error| match error.kind {
                    ErrorKind::Input => Failure::Input,
                    ErrorKind::Budget => Failure::Budget,
                    ErrorKind::Config | ErrorKind::Type => Failure::Config,
                })
            });
            worker.map(ScopedJoinHandle::join)
        });
        let diagnostics = match outcome {
            Ok(Ok(result)) => result?,
            Ok(Err(_)) => {
                self.engines.remove(format);
                return Err(Failure::Internal);
            }
            Err(_) => return Err(Failure::Internal),
        };

        let mut result = Vec::new();
        for edit in diagnostics.edits.unwrap_or_default() {
            let span = body.start + edit.start..body.start + edit.end;
            let inside = |part: &Range<usize>| scope.start <= part.start && part.end <= scope.end;
            result.extend(refine(units, span, &edit.text).into_iter().filter(|replace| inside(&replace.span)));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn typesetter() -> Typesetter {
        Typesetter::new(Settings::default()).unwrap()
    }

    fn apply(text: &[u16], edits: &[Replace]) -> String {
        let mut output = text.to_vec();
        for edit in edits.iter().rev() {
            output.splice(edit.span.clone(), edit.text.iter().copied());
        }
        String::from_utf16(&output).unwrap()
    }

    fn whole(text: &str, format: &'static str) -> String {
        let input = units(text);
        let edits = typesetter().edits(&input, format, None).unwrap();
        apply(&input, &edits)
    }

    fn select(text: &str, part: &str, format: &'static str) -> String {
        let byte = text.find(part).unwrap();
        let start = text[..byte].encode_utf16().count();
        let span = start..start + part.encode_utf16().count();
        let input = units(text);
        let edits = typesetter().edits(&input, format, Some(span)).unwrap();
        apply(&input, &edits)
    }

    #[test]
    fn survives_deep_nesting() {
        let depth = 2_499_980;
        let cases = [
            ("json", format!("{}\"Амбер - город\"{}", "[".repeat(depth), "]".repeat(depth))),
            ("toml", format!("{}b = \"Амбер - город\"\n", "a.".repeat(depth))),
            ("yaml", format!("a: {}\"Амбер - город\"{}\n", "[".repeat(depth), "]".repeat(depth))),
            ("frontmatter", format!("+++\n{}b = \"Амбер - город\"\n+++\n", "a.".repeat(depth))),
        ];
        let mut typesetter = typesetter();
        for (format, text) in cases {
            let edits = typesetter.edits(&units(&text), format, None).unwrap();
            assert_eq!(edits.len(), 2, "{format}");
        }
    }

    #[test]
    fn typesets_a_whole_document() {
        assert_eq!(whole("\"Амбер\" - город", "plain"), "«Амбер»\u{a0}— город");
    }

    #[test]
    fn keeps_blank_edges_of_a_document() {
        assert_eq!(whole("\n\n\"Амбер\" - город\n\n", "plain"), "\n\n«Амбер»\u{a0}— город\n\n");
        assert_eq!(whole("  \n\t\n", "plain"), "  \n\t\n");
    }

    #[test]
    fn keeps_indent_of_first_line_for_yaml() {
        assert_eq!(
            whole("  title: Амбер - город\n  author: Корвин\n", "yaml"),
            "  title: Амбер\u{a0}— город\n  author: Корвин\n"
        );
    }

    #[test]
    fn protects_code_and_frontmatter_keys() {
        let text = "---\ntitle: Амбер - город\n---\n\nКорвин - принц и `код -- тут`.\n";
        let expected = "---\ntitle: Амбер\u{a0}— город\n---\n\nКорвин\u{a0}— принц и\u{a0}`код -- тут`.\n";
        assert_eq!(whole(text, "frontmatter"), expected);
    }

    #[test]
    fn selection_changes_nothing_outside() {
        let text = "Корвин - принц. Рэндом - король. Блейз - брат.";
        assert_eq!(select(text, "Рэндом - король", "plain"), "Корвин - принц. Рэндом\u{a0}— король. Блейз - брат.");
    }

    #[test]
    fn selection_ending_on_a_quote_splits_the_merged_edit() {
        assert_eq!(select("\"Амбер\" - город", "\"Амбер\"", "plain"), "«Амбер» - город");
    }

    #[test]
    fn selection_keeps_its_edge_whitespace() {
        let text = "Корвин:  Амбер - город  конец";
        assert_eq!(select(text, "  Амбер - город  ", "plain"), "Корвин:  Амбер\u{a0}— город  конец");
    }

    #[test]
    fn selection_in_json_keeps_structural_quotes() {
        let text = "{\"title\": \"Амбер - город\", \"author\": \"Корвин - принц\"}\n";
        assert_eq!(
            select(text, "\"title\": \"Амбер - город\"", "json"),
            "{\"title\": \"Амбер\u{a0}— город\", \"author\": \"Корвин - принц\"}\n"
        );
    }

    #[test]
    fn selection_inside_code_fence_changes_nothing() {
        let text = "Текст\n\n```\nАмбер - город\n```\n";
        assert_eq!(select(text, "Амбер - город", "frontmatter"), text);
    }

    #[test]
    fn empty_selection_changes_nothing() {
        let input = units("Амбер - город");
        assert_eq!(typesetter().edits(&input, "plain", Some(3..3)).unwrap(), [] as [Replace; 0]);
        assert_eq!(typesetter().edits(&input, "plain", Some(5..6)).unwrap(), [] as [Replace; 0]);
    }

    #[test]
    fn surrogate_pairs_do_not_shift_edits() {
        assert_eq!(
            select("😀 Корвин - принц 😀 Рэндом - король", "Рэндом - король", "plain"),
            "😀 Корвин - принц 😀 Рэндом\u{a0}— король"
        );
    }

    #[test]
    fn keeps_crlf_line_endings() {
        assert_eq!(
            whole("Амбер - город\r\nКорвин - принц\r\n", "plain"),
            "Амбер\u{a0}— город\r\nКорвин\u{a0}— принц\r\n"
        );
        assert_eq!(whole("\"Амбер\"\r\n\"Корвин\"\r\n", "plain"), "«Амбер»\r\n«Корвин»\r\n");
    }

    #[test]
    fn is_idempotent() {
        for (text, format) in [
            ("\"Амбер\" - город\n", "plain"),
            ("# Корвин\n\n\"Амбер\" -- вечен, 3000 лет.\n", "frontmatter"),
            ("{\"a\": \"Амбер - город\"}\n", "json"),
        ] {
            let once = whole(text, format);
            assert_ne!(once, text);
            assert_eq!(whole(&once, format), once);
        }
    }

    #[test]
    fn malformed_structured_document_fails_closed() {
        let input = units("{\"a\": ");
        assert_eq!(typesetter().edits(&input, "json", None).unwrap_err(), Failure::Input);
    }

    #[test]
    fn input_over_the_limit_fails_closed() {
        let options = json!({ "maxInputLength": 5 });
        let settings = config::read(Some(&options)).unwrap();
        let mut typesetter = Typesetter::new(settings).unwrap();
        let input = units("Амбер - город");
        assert_eq!(typesetter.edits(&input, "plain", None).unwrap_err(), Failure::Input);
    }

    #[test]
    fn engine_rejects_unknown_rule_at_startup() {
        let options = json!({ "rules": { "no.such.rule": true } });
        let settings = config::read(Some(&options)).unwrap();
        assert_eq!(Typesetter::new(settings).unwrap_err(), Failure::Config);
    }

    #[test]
    fn presets_off_changes_nothing() {
        let options = json!({ "presets": false });
        let settings = config::read(Some(&options)).unwrap();
        let mut typesetter = Typesetter::new(settings).unwrap();
        let input = units("\"Амбер\" - город");
        assert_eq!(typesetter.edits(&input, "plain", None).unwrap(), [] as [Replace; 0]);
    }
}
