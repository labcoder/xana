# Terminal presentation

Xana resolves terminal presentation once when a terminal surface starts. The
resolved profile affects only rendering: it cannot change the selected
connection or model, reasoning, tools, permissions, activity meaning, or agent
authority.

The append-only terminal remains usable when styling is unavailable. Captured
or redirected output, `TERM=dumb`, `NO_COLOR`, and the monochrome theme contain
no ANSI styling or cursor-control sequences. Statuses and approval choices
always have text labels, so color is never their only meaning.

## Automatic behavior

Xana uses these conservative inputs at process startup:

- `NO_COLOR` disables color.
- `COLORTERM` containing `truecolor` selects truecolor; otherwise a `TERM`
  containing `256color` selects 256 colors and an interactive terminal falls
  back to 16 colors.
- `COLORFGBG` may identify a light or dark background. When no background is
  available, `auto` uses the dark palette without querying the terminal.
- `LC_ALL` or `LANG` containing `UTF-8`/`UTF8` enables Unicode on Unix-like
  systems. Windows terminals use Unicode by default. ASCII remains available
  as an explicit override.
- `COLUMNS` selects the wide, compact, or narrow banner layout when it is a
  plausible value. An unavailable width uses 80 columns.
- A truthy `XANA_REDUCED_MOTION` selects reduced motion. The TUI consumes this
  resolved value; the append-only surface has no animation.

These checks are local and immediate. Xana does not issue terminal capability
queries that could delay or hang startup.

## Machine-local preferences

Presentation preferences are separate from `config.toml`. The version 1 file
is `data/frontend/presentation.toml` under `XANA_HOME`; platform-default
installations place the same file under Xana's platform data directory.
Full Custom Setup and the focused appearance section manage this file:

```bash
xana setup --section appearance
xana setup --non-interactive --section appearance \
  --theme monochrome --glyphs ascii --motion reduced \
  --density compact --composer newline --activity hidden --yes
```

It can also be created by hand:

```toml
version = 1
theme = "auto"       # auto, dark, light, monochrome
glyphs = "auto"      # auto, unicode, ascii
motion = "auto"      # auto, full, reduced
density = "auto"     # auto, comfortable, compact
composer = "submit"  # submit, newline
```

The file is read with a 32 KiB limit and rejects unknown fields or unsupported
versions. A missing file means defaults. Invalid or oversized preferences
produce an actionable warning and safe defaults without changing agent
configuration.

Xana owns semantic presentation tokens for accent, muted, success, warning,
danger, user, assistant, summary, reasoning, tool, child, focus, approval, and
diff additions/removals. Frontends choose concrete colors from the resolved
profile rather than embedding presentation codes in runtime or domain logic.

The `composer` preference is also presentation-owned machine-local state. Use
`/composer submit` or `/composer newline` in the TUI to persist it atomically;
see [Full-screen terminal UI](tui.md) for the portable Ctrl+J alternate.
