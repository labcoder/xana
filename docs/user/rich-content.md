# Rich terminal content and artifacts

> Audience: People reading long conversations or working with Xana artifacts.

The full-screen TUI renders a bounded Rust-native subset of Markdown. It
supports paragraphs, headings, emphasis, inline code, lists, quotes, pipe
tables, fenced code, and diff fences. Unsupported or malformed input remains
readable text. Styling degrades through the same dark/light, limited-color,
monochrome, Unicode, ASCII, and no-color presentation profiles as the rest of
the TUI.

All model, tool, managed-runtime, and artifact preview text is untrusted.
Before it reaches Ratatui, Xana removes ANSI/OSC and C0/C1 terminal controls,
normalizes line endings, neutralizes bidirectional control characters, bounds
source/line/link counts, and accepts only inert `http`, `https`, and `mailto`
link metadata. Rendering a link never opens it or writes a terminal hyperlink.

The conversation renderer draws only a viewport-sized message window. Native
historical sessions are indexed by entry identity and byte offset; each
explicit older-page request reads at most 128 messages rather than retaining
the complete transcript in the frontend. The TUI retains at most 512 projected
messages and adjusts its scroll anchor when an older page is inserted.
Streaming at the newest edge remains anchored there; scrolling up preserves
the viewed region while later messages arrive.

## Artifacts and images

Images and other artifacts remain immutable content-addressed references.
Conversation snapshots contain bounded metadata, not embedded binary bytes or
arbitrary paths. An image line reports its artifact id, media type, and byte
length; it does not claim terminal image-protocol support.

Use `/artifact ARTIFACT_ID` for an artifact already visible in the bounded
conversation view. The action card offers:

- a 64 KiB text/JSON/TOML preview, or metadata-only binary preview;
- insertion of the immutable `artifact:ID` reference into the draft;
- explicit reveal in the OS file manager; or
- explicit open with the OS default application.

Nothing opens automatically during rendering, selection, resize, or preview.
Before reveal/open Xana re-verifies the content-addressed file and declared
size inside its artifact store. Missing, corrupt, oversized, inaccessible, or
non-UTF-8 content produces a bounded error. The current terminal surface does
not copy through OSC 52 or place data on the system clipboard; “insert
reference” keeps the operation visible and portable.

## Bounds

- Rich source: 1 MiB per projected message.
- Rich lines: 4,096, with 16 KiB per line.
- Links and artifacts: 64 each per message.
- Historical page: 128 messages.
- Retained projected conversation: 512 messages.
- Rendered message window: derived from terminal height, never more than 128.
- Artifact text preview: 64 KiB; binary content is not embedded.

Truncation is labeled. Durable session records and the artifact store remain
authoritative; the rich document, viewport window, and page indexes are
derivative presentation state.
