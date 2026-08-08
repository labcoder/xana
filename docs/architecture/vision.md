# Image input and media resolution

Audience: contributors and coding agents. Authority: descriptive.

Image input is reference-based. `/attach WORKSPACE_RELATIVE_PATH` performs one
bounded, regular-file read after workspace and symlink checks, validates a
small set of image headers and pixel limits, and publishes immutable bytes to
the artifact store. The pending attachment queue is separate from durable
conversation history: `/clear` removes pending attachments with a visible
confirmation, and accepting a turn consumes the queue exactly once.

The internal message model carries `ContentBlock::Image` references rather
than bytes. `xana-core::ModelCapabilities` provides pre-transport checks for
input modality, image count, image-byte budget, context limit, and tool support.
`MediaResolver` reads a verified artifact only at the provider wire edge and
can produce a bounded OpenAI-compatible data URL; it never persists that
encoded representation.

The current Anthropic adapter rejects image blocks explicitly until the
provider's image block conversion is accepted and covered by the shared media
suite. There is no URL fetching, image generation, OCR, Files API, automatic
model routing, or terminal graphics path.
