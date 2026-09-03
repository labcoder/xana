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

## Shared content and fallback contract

The frontend-neutral boundary recognizes bounded text and Markdown, whole
fenced code or diff blocks, pipe tables, display-math source, safe HTTP(S)
links, and immutable Xana resources. Ambiguous or malformed markup stays text
instead of being guessed into an executable or interactive form. Tool-call
arguments are not copied into the shared rich-content projection.

Each interface chooses one honest presentation tier: rich, text, metadata, or
unsupported. Every tier retains a bounded readable fallback. The current TUI
keeps its established terminal renderer while the remaining M4 interface work
adopts this shared projection; equal semantics do not require equal pixels.

Desktop preserves the typed parts instead of flattening them into one string.
It renders bounded Markdown, code, tables, and diffs through the selectable
`gpui-ai` transcript. A second Desktop adapter reparses model-authored Markdown
and removes raw HTML/MDX, remote images, credentials, fragments, and unsafe URL
schemes before it reaches a clickable renderer. Math remains readable LaTeX
source rather than an advertised formula renderer. Accepted static PNG, JPEG,
and WebP resources may gain verified thumbnails; every other resource keeps a
typed metadata card. See [Desktop rich content](desktop.md#rich-content-and-artifacts).

## Artifacts and media

Images and other artifacts remain immutable content-addressed references.
Conversation snapshots contain bounded metadata, not embedded binary bytes or
arbitrary paths. An image line reports its artifact id, media type, and byte
length. With `appearance.inline_image = "auto"`, the TUI may also render a
bounded preview after positively detecting a supported terminal protocol and
dimensions. `"off"`, an unproven multiplexer, an unsupported terminal, or a
failed decode always uses the metadata fallback. Terminal presentation never
implies model input support.

Xana's resource vocabulary covers static and animated raster images, SVG,
Lottie, audio, video, binary, and safe unknown kinds. Bounded signature
inspection identifies common PNG/JPEG/GIF/WebP, SVG, Lottie JSON, WAV/MP3/Ogg,
WebM, and MP4 containers while keeping declared and detected types separate.
Identification is not permission and does not promise that the active
interface can render, play, transform, or send the resource. SVG and Lottie
stay pending for reviewed safe-derivative adapters; unknown binaries are not
rendered or sent.

The TUI can stage multiple typed local resources with `/attach PATH`, terminal
paste/file-drop paths, or `/attach --clipboard` for supported clipboard images.
Use `/attach list` and `/attach clear` to inspect or remove the pending set.
Files inside the workspace use workspace authority; an external path requires
an exact allow-once decision before bytes are read. Acquisition validates the
configured aggregate policy and compiled ceiling, streams the full file into
the immutable store, and retains only a bounded signature probe in memory.
Current provider routes send only validated PNG/JPEG/GIF inputs to an exact
image-capable model. Other resource kinds retain metadata and artifact actions,
but fail closed before provider disclosure until a compatible route exists.
Desktop applies the same admission contract to its picker and drag/drop paths.
Unsupported route inputs remain visible as `retained only` draft cards, so a
failed submission can be corrected without reacquiring the local resource.

Use `/artifact ARTIFACT_ID` for an artifact already visible in the bounded
conversation view. The action card offers:

- a 64 KiB text/JSON/TOML preview, or metadata-only binary preview;
- insertion of the immutable `artifact:ID` reference into the draft;
- explicit reveal in the OS file manager; or
- explicit open with the OS default application.

Nothing opens automatically during rendering, selection, resize, or preview.
Before a bounded range, reveal, or open, Xana re-verifies the complete
content-addressed file's length and digest and rejects a symlink, non-regular
file, or replacement. Desktop save uses a native destination picker and streams
a verified, create-new copy; it never overwrites an existing path. Local-host
range requests name an opaque artifact ID and offset, never a path, and retain
at most 64 KiB. Missing, corrupt, oversized, inaccessible, or non-UTF-8 content
produces a bounded error.

The current terminal surface does not copy artifacts through OSC 52. “Insert
reference” keeps artifact operations visible and portable. A separate explicit
mouse drag over visible conversation text retains a bounded rendered-cell
selection; Ctrl+C explicitly copies that selection to the platform text
clipboard.

## Links and previews

Showing a link does not contact it. Link preview and operating-system open are
separate explicit actions. Native `web_fetch` is the implemented runtime-owned
preview boundary: it requires exact outbound review, accepts public HTTPS text,
checks every address and redirect, and returns a sanitized generic card. See
[Native web fetch](web-fetch.md) for exact limits and privacy behavior.

Xana does not embed a remote page or execute its HTML, JavaScript, CSS, SVG,
forms, or subresources. A failed preview leaves the original safe link useful.

## Capabilities and summaries

Acquisition, inline presentation, playback, provider input, focused analysis,
transformation, and external open are independent resource facts. Exact
provider/model/route facts retain their source, freshness, effective byte
limit, and decision reason. An absent fact stays unsupported rather than being
inferred from a file extension.

Xana can project deterministic summaries from runtime facts and existing
provider or compaction summaries without another model call. A fresh model
recap is separate explicit intent naming its connection, model, freshness, and
usage effect; Xana does not silently spend tokens to summarize every turn.

## Bounds and current limits

- Rich source: 1 MiB per projected message.
- Rich lines: 4,096, with 16 KiB per line.
- Links and artifacts: 64 each per message.
- Historical page: 128 messages.
- Retained projected conversation: 512 messages.
- Rendered message window: derived from terminal height, never more than 128.
- Explicit visible conversation selection: 256 Ki terminal cells, then bounded
  again to the 1 MiB projected-message limit before clipboard delivery.
- Artifact range: at most 64 KiB retained while the whole source is verified.
- Desktop inline previews: at most 8 eligible static images and 20 MiB of
  aggregate source bytes retained at once.
- Current image input: 8 images, 4 MiB per image, 20 MiB total source bytes per
  turn, and 40 million decoded pixels per image.

Truncation is labeled. Durable session records and the artifact store remain
authoritative; rich documents, viewport windows, page indexes, and resource
metadata are derivative state. In-app audio/video playback, safe SVG
rasterization, animated-image presentation, native Lottie rendering, and broad
provider upload support are not implied by detection. Unsupported content
retains its safe metadata/text fallback and only the explicit actions the
active interface actually advertises.
