use std::collections::BTreeSet;

use microtypo_core::Config;
use serde_json::Value;

use crate::detect::language_id;
use crate::document::MAX_UNITS;

const MAX_RULES: usize = 1024;
const MAX_PROCESSING_MS: u64 = 30_000;
const DEFAULT_PROCESSING_MS: f64 = 5_000.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub format: BTreeSet<&'static str>,
    pub engine: Config,
}

impl Default for Settings {
    fn default() -> Self {
        let engine = Config {
            html: Some(false),
            max_input_length: u32::try_from(MAX_UNITS).ok(),
            max_processing_ms: Some(DEFAULT_PROCESSING_MS),
            ..Config::default()
        };
        Self { format: BTreeSet::new(), engine }
    }
}

fn bounded(value: &Value, max: u64) -> Option<u32> {
    value.as_u64().filter(|number| (1..=max).contains(number)).and_then(|number| u32::try_from(number).ok())
}

fn read_format(value: &Value) -> Option<BTreeSet<&'static str>> {
    value.as_array()?.iter().map(|name| language_id(name.as_str()?)).collect()
}

fn read_rules(value: &Value) -> Option<Vec<(String, bool)>> {
    let rules = value.as_object()?;
    if rules.len() > MAX_RULES {
        return None;
    }
    rules.iter().map(|(rule, on)| Some((rule.clone(), on.as_bool()?))).collect()
}

pub fn read(options: Option<&Value>) -> Result<Settings, &'static str> {
    let mut settings = Settings::default();
    let fields = match options {
        None | Some(Value::Null) => return Ok(settings),
        Some(Value::Object(fields)) => fields,
        Some(_) => return Err("options must be an object"),
    };
    let engine = &mut settings.engine;
    for (key, value) in fields {
        match key.as_str() {
            "format" => settings.format = read_format(value).ok_or("invalid format list")?,
            "presets" => engine.presets = Some(value.as_bool().ok_or("invalid presets")?),
            "rules" => engine.rules = read_rules(value).ok_or("invalid rules")?,
            "maxInputLength" => {
                engine.max_input_length = Some(bounded(value, MAX_UNITS as u64).ok_or("invalid maxInputLength")?);
            }
            "maxProcessingMs" => {
                let limit = bounded(value, MAX_PROCESSING_MS).ok_or("invalid maxProcessingMs")?;
                engine.max_processing_ms = Some(f64::from(limit));
            }
            _ => return Err("unknown option"),
        }
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn absent_options_register_no_formatter() {
        for options in [None, Some(&Value::Null)] {
            let settings = read(options).unwrap();
            assert!(settings.format.is_empty());
            assert_eq!(settings.engine.html, Some(false));
            assert_eq!(settings.engine.max_input_length, Some(5_000_000));
            assert_eq!(settings.engine.max_processing_ms, Some(5000.0));
        }
    }

    #[test]
    fn reads_every_option() {
        let options = json!({
            "format": ["Markdown", "Plain Text"],
            "presets": false,
            "rules": { "quote": true },
            "maxInputLength": 1000,
            "maxProcessingMs": 250,
        });
        let settings = read(Some(&options)).unwrap();
        assert_eq!(settings.format, BTreeSet::from(["markdown", "plaintext"]));
        assert_eq!(settings.engine.presets, Some(false));
        assert_eq!(settings.engine.rules, [("quote".to_owned(), true)]);
        assert_eq!(settings.engine.max_input_length, Some(1000));
        assert_eq!(settings.engine.max_processing_ms, Some(250.0));
    }

    #[test]
    fn keeps_rules_in_the_given_order() {
        let options = json!({ "rules": { "quote": false, "nbsp": true, "dash": false } });
        let settings = read(Some(&options)).unwrap();
        let names: Vec<_> = settings.engine.rules.iter().map(|(rule, _)| rule.as_str()).collect();
        assert_eq!(names, ["quote", "nbsp", "dash"]);
    }

    #[test]
    fn rejects_invalid_options() {
        let cases = [
            json!([]),
            json!({ "unknown": 1 }),
            json!({ "html": true }),
            json!({ "format": "Markdown" }),
            json!({ "format": ["Rust"] }),
            json!({ "format": [1] }),
            json!({ "presets": "yes" }),
            json!({ "rules": { "quote": "on" } }),
            json!({ "maxInputLength": 0 }),
            json!({ "maxInputLength": 5_000_001 }),
            json!({ "maxProcessingMs": 0 }),
            json!({ "maxProcessingMs": 30_001 }),
            json!({ "maxProcessingMs": 1.5 }),
        ];
        for options in cases {
            assert!(read(Some(&options)).is_err(), "{options}");
        }
    }
}
