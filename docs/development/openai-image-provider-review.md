# OpenAI image provider review

> Reviewed: 2026-08-12  
> Scope: M3-19 direct `image.generate` adapter  
> Authority: implementation evidence, not a replacement for vendor docs

Xana's first focused image adapter uses the official API-key Images API, not a
ChatGPT/Codex subscription token or private backend. The supported operation is
`POST /v1/images/generations` with `gpt-image-2` or a pinned
`gpt-image-2-*` snapshot.

Primary sources reviewed:

- [Create image API reference](https://developers.openai.com/api/reference/resources/images/methods/generate)
  defines Bearer API-key authentication, prompt/options, base64 GPT-image
  results, usage fields, streaming, and errors.
- [GPT Image 2 model](https://developers.openai.com/api/docs/models/gpt-image-2)
  identifies the current image model, image generation/edit endpoints,
  modalities, snapshots, and account-tier rate limits.

## Frozen Xana subset

- Text-to-image generation only. Editing/input images remain disabled until a
  separately tested multipart edit path exists.
- Exactly one final image per call. Xana never retries an accepted or ambiguous
  generation request.
- Options: `size`, `quality`, `output_format`, `background`, `moderation`, and
  `output_compression`. Unknown options fail before network I/O.
- `size` accepts `auto` or a bounded GPT Image 2 `WIDTHxHEIGHT`: divisible by
  16, 256–3840 by 256–2160, and aspect ratio between 1:3 and 3:1.
- Formats are PNG, JPEG, or WebP. The decoded bytes must match the selected
  format signature and Xana's immutable artifact-size limit.
- Usage tokens are recorded when returned. The endpoint does not return an
  authoritative monetary charge, so Xana reports cost as unavailable rather
  than estimating it from mutable pricing.
- No redirect, inherited proxy, response URL, partial image, or provider error
  text crosses the adapter boundary. Requests have a 120-second deadline and
  an 8 MiB wire bound. Cancellation drops Xana's owned request; the provider
  has no separate cancellation proof for this synchronous call.

Deterministic fake-server tests are the CI authority. A live smoke is optional
and requires an explicitly configured API-key route; it must report only route,
model, usage availability, and artifact identity, never prompt or credential.
