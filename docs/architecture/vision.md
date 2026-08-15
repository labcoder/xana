# Image input and media resolution

> Audience: Contributors and coding agents
> Authority: Descriptive

`/attach WORKSPACE_RELATIVE_PATH` performs a bounded regular-file read after
workspace and symlink checks, validates PNG/JPEG/GIF content and pixel limits,
and publishes immutable bytes to Xana's artifact store. Ordinary message text
is also scanned lexically for image-looking paths in input order; resolution
and authority still happen after that untrusted-text detection. The pending queue
preserves attachment order, accepts at most eight images and 20 MiB per turn,
is cleared visibly by `/clear`, and is consumed when a turn is submitted.
The TUI maps a terminal-pasted single image path from drag-and-drop into the
same ingestion operation. Absolute, `file://`, and Git Bash path forms are
normalized before resolution. Workspace images follow ordinary workspace
policy. An image outside the launch workspace is never read ambiently: the
interactive frontend lists all requested external images in one exact
allow-once decision, then imports bounded immutable artifact copies. Every
path is classified before the turn is submitted; denial or validation failure
restores the complete draft rather than sending a partial image set.

The internal message model carries `ContentBlock::Image` references rather
than paths or base64. Before native provider I/O, Xana requires the selected
model descriptor to contain the `image` input modality. Every artifact is
read through its declared bound and verified content hash only at the wire
edge:

- OpenAI-compatible, OpenAI, and OpenRouter use ordered `image_url` data-URL
  content parts;
- Anthropic uses ordered base64 image source blocks; and
- managed Codex receives a verified Xana artifact path. Codex retains
  responsibility for its provider encoding and never receives the original
  external source path.

Encoded bytes and source paths are not written into Xana's native conversation
records. Image capabilities fail closed when catalog evidence or an explicit
override is absent. There is no URL fetching, OCR, image generation, Files API
upload, or terminal image rendering.
