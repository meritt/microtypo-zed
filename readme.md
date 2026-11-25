# MicroTypo for Zed

Russian typography in [Zed](https://zed.dev): guillemets, dashes, non-breaking spaces and signs, for a whole file or a selection. Code, URLs, markup and document structure stay intact. Engine: [microtypo.rs](https://github.com/meritt/microtypo.rs).

```text
Корвин сказал: "Амбер вечен" -- и с этим не поспоришь.
Корвин сказал: «Амбер вечен» — и с этим не поспоришь.
```

## Installation

`zed: extensions` → **MicroTypo**. The server is downloaded on first launch and verified by SHA-256. No Node.js required.

| Language | Engine format |
|---|---|
| Markdown | `frontmatter` |
| MDX\* | `mdx` |
| HTML\* | `html` |
| JSON | `json` |
| YAML | `yaml` |
| TOML\* | `toml` |
| XML\* | `xml` |
| Plain Text | `plain` |

\* Requires the Zed language extension.

## Commands

| Scope | Run | Setup |
|---|---|---|
| Selection | `cmd-.` → **Microtypo selection** | none |
| File | `cmd-.` → **Microtypo file** | none |
| File | `editor::Format` | `format`, `formatter` |
| Selection | `editor::FormatSelections` | `format`, `formatter` |

On Linux and Windows the menu opens with `ctrl-.`; in the command palette it is `editor: toggle code actions`.

## Key bindings

`settings.json`:

```jsonc
{
  "lsp": {
    "microtypo": {
      "initialization_options": { "format": ["Markdown", "Plain Text"] }
    }
  },
  "languages": {
    "Markdown": { "formatter": { "language_server": { "name": "microtypo" } } },
    "Plain Text": { "formatter": { "language_server": { "name": "microtypo" } } }
  }
}
```

`keymap.json`:

```jsonc
[
  {
    "context": "Editor && mode == full",
    "bindings": {
      "cmd-alt-t": "editor::Format",
      "cmd-alt-shift-t": "editor::FormatSelections"
    }
  }
]
```

After Prettier:

```jsonc
{
  "languages": {
    "Markdown": {
      "formatter": ["prettier", { "language_server": { "name": "microtypo" } }]
    }
  }
}
```

Languages outside `format` are never touched by `editor::Format` or on save. With `format_on_save`, a language in `format` is typeset on every save.

## Settings

`lsp.microtypo.initialization_options`:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `format` | `string[]` | `[]` | languages for `editor::Format` and `editor::FormatSelections` |
| `presets` | `boolean` | `true` | built-in rule groups |
| `rules` | `object` | `{}` | [engine rules](https://github.com/meritt/microtypo/blob/main/api/rules-reference.md) |
| `maxInputLength` | `integer` | `5000000` | document length in UTF-16 code units, 1 to 5,000,000 |
| `maxProcessingMs` | `integer` | `5000` | time budget per run in ms, 1 to 30,000 |

Invalid settings fall back to the defaults with the formatter disabled; the reason is in the server log.

## Limitations

- A selection is parsed in the context of the whole document, so code and JSON keys inside it stay intact.
- A quote pair crossing the selection boundary changes only on the selected side.
- Invalid JSON, YAML, TOML or XML is left unchanged.
- Zed disables language servers for files with a line longer than `max_buffer_line_length` (20,000 characters).
- Files not saved to disk are not processed.

## Author

[Alexey Simonenko](https://simonenko.xyz)

## License

MIT. See `LICENSE`.
