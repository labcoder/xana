# Image input and media resolution

> Audience: Contributors and coding agents
> Authority: Descriptive

`/attach WORKSPACE_RELATIVE_PATH` performs a bounded regular-file read after
workspace and symlink checks, validates PNG/JPEG/GIF content and pixel limits,
and publishes immutable bytes to Xana's artifact store. The pending queue
preserves attachment order, accepts at most eight images and 20 MiB per turn,
is cleared visibly by `/clear`, and is consumed when a turn is submitted.

The internal message model carries `ContentBlock::Image` references rather
than paths or base64. Before native provider I/O, Xana requires the selected
model descriptor to contain the `image` input modality. Every artifact is
read through its declared bound and verified content hash only at the wire
edge:

- OpenAI-compatible, OpenAI, and OpenRouter use ordered `image_url` data-URL
  content parts;
- Anthropic uses ordered base64 image source blocks; and
- managed Codex receives canonical local image paths already proven to remain
  below the selected workspace. Codex retains responsibility for its provider
  encoding.

Encoded bytes and source paths are not written into Xana's native conversation
records. Image capabilities fail closed when catalog evidence or an explicit
override is absent. There is no URL fetching, OCR, image generation, Files API
upload, or terminal image rendering.
