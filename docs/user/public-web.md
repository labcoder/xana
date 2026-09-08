# Public web: search, page reading, and browser control

> Audience: Xana users
> Authority: Descriptive

Native Xana has three distinct tools. `web_search` discovers sources;
`web_fetch` reads a known public HTTPS page without a browser; `browser`
reads or controls a dedicated browser. The conversational model chooses tools
and writes the answer. None of these routes calls a hosted Answers or deep
research service. Managed Codex retains its own tools and account policy.

## Choose a search connection

Search is disabled until you choose a route, independently of your chat model.
From a checkout, prefix commands below with `cargo run --` instead of `xana`.

```text
xana connect web
xana connect web --web-provider exa --credential-env EXA_API_KEY --yes
xana connect web --web-provider brave --credential-env BRAVE_API_KEY --yes
xana connect web --web-provider exa-mcp --yes
xana connect web --web-provider pages-only --yes
xana doctor
```

Interactive API setup accepts a hidden key and stores it through Xana's OS
credential store, or accepts an environment-variable reference. Never pass the
key value as a CLI argument or put it in `config.toml`. Exa hosted MCP is an
explicit no-key service choice with provider-controlled availability/rate limits,
not an unlimited entitlement or an automatic fallback. Use Exa API for your own
Exa key. Xana does not probe or bill a service during setup or doctor.

`--service-connection NAME` names the search connection. Setup retains other
search and chat connections; select another route by setting it up under its
existing name. `--web-provider disabled` disables discovery without deleting
connections or preventing known-page reads. Existing native Conversations adopt
web changes before their next new turn; a new Conversation is not required.
Doctor distinguishes local readiness from remote validation.

Setup also reviews enabling `prompt_text` disclosure in the default Profile.
Existing Conversations using that Profile adopt the owner-approved change at
a safe turn boundary, preserving their history and original Profile record.
Other named Profiles must be configured explicitly. If web tools say the Profile
disallows disclosure, review its configuration and retry here. Permission to
send public queries is still requested separately; enabling a capability is
not the same as granting it unrestricted authority.
`pages-only` enables known-page reading without selecting a search provider.
Changing to `ask` or disabling search does not broaden any Profile's policy.

## Fewer repeated approvals, without browser authority

A web approval offers **Allow once** and **Allow public web for this turn**.
The second covers bounded queries to the selected search route and public HTTPS
reads, including checked redirects, in this one owner turn. Plain-terminal mode
uses `w`; TUI and Desktop show the same choice. Existing exact saved decisions
remain exact, and saved denies still win. Restarting does not restore turn grants.

To choose a persistent preference separately:

```text
xana connect web --public-web allow --yes
xana connect web --public-web ask --yes
```

This permission sends model-generated query/URL contents to public recipients;
Xana does not promise semantic detection of secrets inside a query. It does
**not** grant browser actions, private-network access, local-file/artifact
uploads, cookies, arbitrary HTTP/MCP operations, or permission to switch search
providers. API credentials go only to the explicitly selected search endpoint.
Read [outbound data](outbound-data.md) for exact decisions and revocation.

## Bounds and failures

```toml
[web]
default_connection = "brave"
public_web = "ask"

[web.connections.brave]
provider = "brave"
credential = { source = "environment", variable = "BRAVE_API_KEY" }

[web.limits]
searches = 3
attempts = 8
concurrency = 2
ingress_bytes = 16777216
fetch_bytes = 2097152
```

The root turn shares these counters across search and fetch. Redirects and MCP
handshake traffic count, including failed attempts; they are not success-only
counters. A stateless Exa MCP search uses three wire attempts, so the default
eight-attempt allowance may stop before a third search. Provider-internal work
and pricing are not observable here. Limits can be lowered or raised up to
compiled ceilings: 8 searches, 24 attempts, 4 concurrent operations, 64 MiB
ingress and 4 MiB per fetched page.

Queries are capped at 2 KiB, search receipts at 24 KiB, and search calls at 25
seconds. Fetch defaults to 20 seconds, accepts UTF-8 HTML/plain/Markdown/JSON,
requests identity encoding, and never executes page JavaScript. It retains up
to 24 KiB inline plus an immutable artifact for bounded larger sources.
Identical authorized requests reuse their in-turn receipt; failed or interrupted
searches are not silently resubmitted. There is no cross-turn search cache.
Receipt reuse retains the original retrieval timestamp. HTML rendering rejects
overly deep or complex trees; a timed-out parser retains its concurrency slot
until it exits, including across subsequent turns. This is not a hard OS memory
or CPU sandbox.

Sources retain URLs, retrieval times, truncation and honest provider metadata.
A relative age label is not a fabricated publication timestamp. Exa MCP formatted
text remains unstructured evidence. Empty results, 404s, challenges, missing keys,
rate limits and exhausted allowances are distinct from successful evidence.
Conversation/Activity display actual work stages and elapsed time, not invented
thinking. Model answer quality still varies; a successful HTTP response alone
does not establish that an answer is correct.

## Try it

- “Use web search to find the next Miami Marlins game. Give the date, time zone,
  and source; say if the schedule could not be verified.”
- “Read https://oscarsanchez.com and summarize the page in one sentence.”
- “Use the browser to open https://oscarsanchez.com, summarize the visible page,
  and leave it open for manual takeover. Do not follow links or submit anything.”

The last request uses `browser.open`, which checks readiness, launches if closed,
navigates and returns a bounded observation. See [local browser](local-browser.md)
for native prerequisites, headless/headful behavior, takeover and cleanup.
