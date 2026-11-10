---
name: i18n
description: Write and review user-facing text in the Cursor BYOK desktop app (apps/desktop), including t() usage, placeholders, and the English-only text check.
---

# Desktop text

The desktop interface is English only. There are no locale catalogs and no language setting.

```text
apps/desktop/
├── plugins/static-i18n-plugin.ts   # checks every t() call at build time
└── src/i18n/
    ├── runtime.ts                  # t(): fills {placeholders}
    └── global.d.ts                 # declares the global t
```

## Author messages

- Use the global `t()` function with a static English string literal: `t("Save")`.
- Do not import or locally declare `t`.
- Use named placeholders with a static object: `t("Page {page}", { page })`.
- Wrap all user-visible labels, descriptions, tooltips, empty states, validation messages, and accessibility labels, so every interface string stays easy to find. Do not wrap protocol tokens, model IDs, URLs, or product names.
- Use ASCII punctuation (`:`, `,`, `(`), not full-width forms.

## Build check

From `apps/desktop`, run `npm run check`. The build fails when a `t()` call:

- has text that is not a static string literal,
- contains Chinese characters (text merged from upstream must be translated first),
- has placeholders that do not match its params object.

Plugin display text (`PluginLocalizedText`) may still be a locale map; read it with `pluginText(value, LOCALE)`.
