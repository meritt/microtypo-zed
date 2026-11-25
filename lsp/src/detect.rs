const FORMATS: [(&str, &str); 8] = [
    ("markdown", "frontmatter"),
    ("mdx", "mdx"),
    ("html", "html"),
    ("json", "json"),
    ("yaml", "yaml"),
    ("toml", "toml"),
    ("xml", "xml"),
    ("plaintext", "plain"),
];

pub fn format_of(language: &str) -> Option<&'static str> {
    FORMATS.iter().find(|(id, _)| *id == language).map(|&(_, format)| format)
}

pub fn language_id(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    let name = if name == "plain text" { "plaintext" } else { name.as_str() };
    FORMATS.iter().find(|(id, _)| *id == name).map(|&(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_languages_to_engine_formats() {
        assert_eq!(format_of("markdown"), Some("frontmatter"));
        assert_eq!(format_of("plaintext"), Some("plain"));
        assert_eq!(format_of("json"), Some("json"));
    }

    #[test]
    fn refuses_unknown_languages() {
        assert_eq!(format_of("rust"), None);
        assert_eq!(format_of("jsonc"), None);
        assert_eq!(format_of(""), None);
    }

    #[test]
    fn reads_zed_language_names() {
        assert_eq!(language_id("Markdown"), Some("markdown"));
        assert_eq!(language_id("Plain Text"), Some("plaintext"));
        assert_eq!(language_id("plaintext"), Some("plaintext"));
        assert_eq!(language_id(" YAML "), Some("yaml"));
        assert_eq!(language_id("Rust"), None);
    }
}
