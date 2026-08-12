# Media and document services

> Audience: Contributors and coding agents  
> Authority: None  
> Status: Proposed

## Context

Images, audio, generated artifacts, and structured documents need contracts
different from conversational text and bounded UTF-8 source files. This
proposal introduces focused services and durable references without expanding
the conversational provider interface into unrelated operations.

## Media and artifacts

The immutable content-addressed artifact substrate is implemented separately
through historical
[Proposal 0012](0012-durable-sessions-and-context.md). This proposal remains
Proposed for media blocks, document extraction, and focused media services.

The internal conversation model continues to carry ordered content blocks.
Image input can join text and tool blocks without exposing a provider wire
format to the engine.

The first image-input and bounded text/CSV document-extraction slices are
implemented through historical
[Proposal 0015](0015-connections-models-and-managed-runtimes.md):
artifact-backed image references, native OpenAI-compatible and Anthropic wire
encoding, checked Codex paths, `/attach`, `read_document`, and independent
resource limits. This proposal remains Proposed for audio, generation, OCR,
broader structured formats, parser isolation, and focused media services.

Large binary content is not repeated in session records or command/event
protocols. A runtime-owned store gives durable or disposable media an explicit
lifecycle and exposes a typed `ArtifactRef`. Provider adapters resolve and
encode bytes at the wire edge; frontends fetch or render artifacts by
reference.

The same artifact store is the byte/document substrate for addressable external
context. A context record adds versions, selectors, provenance, trust,
ownership, hashes, and materialization budgets without introducing another
blob store. See
[Proposal 0008](0008-artifact-backed-context-and-native-plans.md).

Image generation is a focused service that an agent may see through a tool.
Text-to-speech observes text output, while transcription produces user input;
neither belongs in the conversational provider trait. Browser and desktop
control are execution backends that combine screenshots with typed actions and
remain subject to capability, authority, and containment checks.

## Document extraction

A focused `DocumentExtractor` service supplies the logical capability
`document.extract`. A separate `read_document` tool consumes that interface.
The bounded UTF-8 `read_file` tool remains extension-agnostic and continues to
handle ordinary source and text.

The current default extractor is registered outside the headless engine and
supports bounded UTF-8 plus CSV-to-Markdown; a minimal build may omit CSV.
AnyDoc or another broader parser remains a future adapter rather than an
implemented dependency. OCR and isolated parsers can implement the same
logical capability so downstream vision, skills, and ingestion do not depend
on one concrete crate.

## Untrusted extraction flow

1. Resolve the path within its scope and evaluate read authority.
2. Open one regular file once, reject unsupported file kinds, and read through
   an input bound.
3. Prefer signatures and container identity over filename extensions; use an
   extension only to refine a compatible or signatureless format.
4. Enforce separate limits for input, archive entries and expansion, parser
   work, extracted output, artifacts, and model context.
5. Return typed unsupported, malformed, limit, and extraction failures, plus
   typed artifact references when assets are retained.

Renaming an executable or arbitrary text to `.docx` does not establish an
Office document's identity. Conversely, misleading extensions do not prevent
ordinary text from being read through `read_file`. Extracted links, macros,
embedded objects, and remote image references remain data: Xana does not
execute, open, or fetch them automatically.

Library parser limits provide defense in depth rather than the complete Xana
resource policy. A hostile-upload posture may eventually require a killable
process or OS boundary because a synchronous in-process parser cannot be
reliably stopped by an async timeout.

Capability lifecycle is developed in
[Proposal 0002](0002-capability-model-and-routing.md); execution authority is
developed in [Proposal 0003](0003-tool-authority-and-execution.md).

## Milestone 3 status review

[Proposal 0021](0021-focused-multimodal-services-and-routing.md) accepts the
exact named image-generation, native-vision, specialist-vision, and clipboard
slice. This proposal remains Proposed for audio, speech, transcription, video,
OCR, broad document/media services, and general parser isolation.

## Open questions

- Which document formats and output structure belong in the first extractor
  contract?
- Which artifacts are durable, disposable, or model-context-only?
- Which extracted structures become reusable context views rather than only
  rendered artifacts?
- How is AnyDoc packaged without forcing it into every build?
- Which parser workloads require process isolation before they are exposed?
