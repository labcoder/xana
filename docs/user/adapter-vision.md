# Selected-image turns through the Desktop facade

Repository-local adapters can use `xana::desktop::DesktopClient` without reaching
into provider, credential, artifact-path, or agent-loop internals. This remains
Xana's repository-private facade, not a separately versioned SDK or public
network API. The GPUI application’s existing image flow is unchanged.

For a controlled self-hosted HTTPS specialist, `DesktopLaunch::with_service_certificate`
accepts one exact HTTPS origin and one DER certificate (at most 64 KiB). The
origin must already exist in the configured service connections. Extra trust
applies only to focused specialist requests to that origin; the native brain,
other origins and the operating system trust store are unchanged. Normal
hostname and chain checks, disabled redirects, egress policy and one-use
approval still apply. This is an explicit per-launch embedding option, not an
insecure-TLS mode or a substitute for configuring a route.

## Plan, review, dispatch

1. Launch a client with an explicit workspace and Xana home. Upload PNG, JPEG,
   or GIF bytes with `stage_image_bytes`, or use an existing staged attachment.
   Wait for `DesktopUpdate::AttachmentStaged`; malformed or unsupported images
   produce a typed rejection, not a provider request.
2. Call `plan_vision_turn(question, attachments, route)`. The ordered attachment
   list is authoritative. `None` preserves the capable brain's native image
   path; a text-only brain resolves its configured vision route. `Some(route)`
   explicitly selects an exposed specialist route, even for a capable brain.
3. Display the `DesktopVisionUpdate::Planned` value: authored question, ordered
   immutable source IDs/digests/sizes, selected connection/model/adapter,
   recipient and approval requirement. Planning performs no provider I/O.
4. Pass the unchanged plan to `decide_vision` with `AllowOnce` or `Deny`.
   Workspace-write collision acknowledgement is a separate argument, not an
   egress approval. The plan is consumed once; changing its public display
   fields does not grant different authority.
5. Observe the typed receipt and ordinary runtime messages. Keep the plan's
   `operation_id()` for cancellation and exact receipt inspection.

The facade uses the existing `VisionTurnService`, focused-service registry,
outbound guard, credential resolver and native runtime. It does not introduce a
second agent, tool loop, permission store, provider retry, or implicit fallback.
Saved denials and profile data ceilings still apply. Unsupported attached or
managed owners reject these facade-specific operations explicitly.

## Evidence and failure meaning

`NativeSubmitted` means the native runtime has durably accepted that exact
operation; it is not a claim that the provider succeeded. Its normal runtime
events and completion receipt report the eventual outcome. Specialist
`AnalysisReady` means the attributed untrusted description was stored before
the ordinary brain continuation. It is not a final answer or a trusted tool
instruction.

The specialist receipt records the immutable ordered sources, prompt digest,
exact plan/recipient identity, revision, derivative artifact and provider usage
when available. Missing usage or cost stays `None`, never fabricated zero.
The original selected images are not resent to a text-only brain. Its input
contains the attributed description; memory learning sees only the original
owner-authored question, and generated text cannot trigger owner-only memory
commands.

Denial, cancellation, controller loss, unsupported routing, unavailable
credentials/services, and analysis failure remain distinct. Cancellation joins
the local request task and prevents brain continuation; it cannot promise that
a remote server discarded a request already received or incurred no charge.
Closing the owning client cancels its in-flight specialist task before stopping
the runtime writer. No receipt authorizes automatic retry.

`inspect_vision_receipt` reads one exact operation in the current Conversation,
including after reopening. A persisted dispatch intent without its terminal
receipt projects as `Unknown`. That is uncertainty, not a failed analysis to
retry automatically. A different Conversation cannot read or approve it.
Newer plans replace older pending plans; controller-generation or configuration
changes invalidate an old approval instead of silently changing its destination.

## Bounds and privacy

Each upload is limited to the existing 4 MiB static-raster limit. Aggregate
queued/in-flight image uploads share a 20 MiB client budget. Turns contain at
most eight ordered images and 20 MiB total, subject to the existing image
dimensions/decode checks. Prompts, route labels, receipt fields and derived text
are bounded. Only one plan or analysis job is owned by the bridge at a time.

Receipt history contains artifact references and digests, not image bytes,
base64, source filesystem paths or credentials. The protected artifact store
contains the selected media and derivative. The question and derivative are
conversation content, not content-free diagnostic metadata. Exporting that
conversation or artifact is a separate owner action.

## Offline consumer verification

Run `cargo test --locked -p xana --test adapter_vision`. The fixture uses the
public facade, local synthetic HTTP servers, fresh workspaces/homes and manual
recovery-key custody in a child process. Specialist fixtures use HTTPS with a
test-only certificate passed through the exact-origin trust option; the native
brain fixture uses the existing local Ollama HTTP route. A wrong-origin case
must fail TLS before image dispatch. It does not contact paid providers,
look up real credentials, import a real browser profile or mutate a user's home.
Native OS custody, live-provider compatibility, visual quality and the retained
manual acceptance checks remain separate qualification gates.
