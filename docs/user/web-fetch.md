# Native web fetch

> Audience: People configuring or using Xana.

Xana's native agent can retrieve one public HTTPS text document with the
typed `web_fetch` tool. The result is also Xana's runtime-owned generic link
preview record. It is a bounded evidence reader, not a browser or web search
engine. Managed Codex owns its own network and tool behavior instead.

Typical requests can be phrased normally:

```text
Read https://example.com/guide and summarize the compatibility notes.
```

The native model chooses whether to call `web_fetch` in response to an explicit
request to read or preview a link. Merely displaying a URL never fetches it.
Before the request,
Xana shows the exact URL chain, purpose, `prompt_text` data class, provenance,
byte estimate, and content digest through the same outbound-data approval used
by MCP, A2A, and focused services. A broad tool setting of `allow` does not
silently approve a new web recipient. Noninteractive use needs an existing
exact saved allow or the separately configured public-web preference; otherwise
it fails closed. An interactive user can also approve public web for this turn.
See [public web](public-web.md) for search setup, Profile eligibility and consent.

## Request and network boundary

The tool accepts only:

- one public HTTPS URL;
- up to three exact redirect destinations;
- a response ceiling from 1 byte through 4 MiB, default 2 MiB (configurable); and
- a whole-request timeout from 1 through 60 seconds, default 20 seconds.

URLs with embedded credentials or fragments are rejected. Xana inherits no
HTTP proxy, cookies, browser profile, authorization header, arbitrary request
header, or ambient credential. Each connection attempt resolves the host,
rejects loopback, private, link-local, metadata, documentation, multicast, and
other special-use addresses, then pins the validated addresses into a
no-redirect client. The same policy applies again after every redirect.

Exact approvals are not widened. Without a public-web turn grant or explicit
persistent public-web preference, on the first unreviewed redirect,
`web_fetch` stops before contacting the destination and returns its canonical
URL. The agent may retry with that URL in the ordered `redirects` argument;
Xana then presents the complete chain for one new exact review. A different,
extra redirect needs that review. A public-web grant permits checked public HTTPS
redirects within the existing three-hop/shared-attempt limits; saved denies still
win. Cyclic or HTTPS-to-HTTP redirects fail closed in either mode.

## Returned evidence

Successful responses must be UTF-8 `text/plain`, `text/markdown`, `text/html`,
`application/xhtml+xml`, or JSON. Xana rejects compressed bodies rather than risk a
decoded-size expansion beyond the reviewed byte ceiling. Header bytes, body
bytes, extraction work, extracted text, and inline model text all have separate
bounds.

Plain text, Markdown and validated JSON remain text. HTML is parsed without JavaScript, CSS,
subresource loading, iframe loading, form submission, or DOM automation and is
rendered to bounded plain text. Control characters are made inert. Every
result reports:

- requested and final URL plus a bounded site name and title derived from the
  sanitized result;
- fetch timestamp and `fresh_not_cached` status;
- MIME type and encoded response length;
- BLAKE3 content digest and ordered redirect provenance;
- bounded extracted text and a truncation fact; and
- `untrusted: true`.

The complete result is a typed, non-executable generic card; it contains no
remote DOM, script, stylesheet, cookie, credential, or navigation authority.
At most 24 KiB of extracted text enters the immediate tool result. When more
source exists, Xana publishes the complete bounded response bytes into its
immutable content-addressed artifact store and returns the opaque artifact
record beside the inline excerpt. HTML or text from a page is evidence only:
it cannot change Xana's identity, grant permission, reveal secrets, or turn
instructions in the page into trusted authority.

## Capability and current limits

The logical capability id is `network.fetch`. Omit `capabilities` from a
profile to use all stock native capabilities, include `network.fetch`
explicitly in a narrowed profile, or omit it to prevent the model from seeing
the tool.

This tool does not provide authenticated-site access, download execution,
JavaScript rendering or conditional revalidation. Separate `web_search` and
`browser` tools provide discovery and approved browser control. Search and fetch
share bounded per-turn attempts/ingress and reuse identical authorized receipts,
including failures, without a persistent cross-turn cache. A reused receipt
retains its original timestamp. See [public web](public-web.md) for the limits.

## Troubleshooting

| Symptom | Meaning and response |
|---|---|
| public HTTPS approval is requested | This exact URL chain has no saved allow. Review the destination and data class. |
| `stopped before an unreviewed redirect` | Retry only if the reported canonical destination is expected; Xana will review the complete chain. |
| `could not be resolved to permitted public addresses` | DNS failed or any resolved address was private or special-use. Do not bypass the check. |
| compressed response rejected | The server did not provide an identity-encoded bounded body. Use another public text representation. |
| unsupported content type or encoding | The resource is not a supported UTF-8 text or JSON form. Use a suitable media/document service. |
| response exceeds its byte limit | Request a smaller public resource; increasing the per-call limit cannot exceed 4 MiB. |
| timeout, unavailable, or cancelled | The operation ended without a complete response. Inspect activity and metadata-only diagnostics before an explicit retry. |

See [Outbound data approvals and privacy](outbound-data.md) for saved decisions
and audit behavior, and [Workspace tools](workspace-tools.md) for local file,
search, and command contracts.
