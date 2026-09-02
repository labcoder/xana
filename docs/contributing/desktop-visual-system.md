# Desktop visual system and component ownership

> Audience: Contributors and coding agents  
> Authority: Repository policy

Xana owns the semantic meaning and restrained, warm character of its native
Desktop. `gpui-ai` owns reusable AI interaction mechanics and
`gpui-component` owns ordinary desktop controls. The application owns all
domain state, stable IDs, clocks, requests, and durable transitions.

## Review the deterministic catalog

The catalog starts without Xana configuration, credentials, a provider, or a
workspace runtime:

```bash
cargo run --locked -p xana-desktop -- --catalog
```

Use it to review component behavior, not final Workbench composition. Its four
pages cover foundations and localization, a real retained conversation and IME
composer, progressive agent work and approvals, and virtualized navigation.
The controls switch light, dark, and high-contrast palettes; compact and
comfortable density; full, reduced, and no motion; 100% and 200% text; English,
representative Spanish, and an expanded pseudolocale.

## Semantic tokens

Raw product colors live only in `crates/xana-desktop/src/design_system.rs`.
Components consume semantic roles so a palette change cannot alter meaning.

| Family | Roles | Contract |
| --- | --- | --- |
| Color | background, surface, foreground, primary, secondary, muted, accent, destructive, success, warning, information, border, input, focus ring, selection | Important state also has a word, icon, or structure; minimum text contrast is tested. |
| Typography | sans and mono families; xs through xl size and line-height tiers | Text scale is bounded from 100% through 200%; content reflows instead of clipping. |
| Spacing | xs through xl | One shared scale governs component rhythm; density adjusts controls without shrinking text. |
| Radius and elevation | semantic surface tiers | High contrast removes decorative shadow and tightens shape without erasing boundaries. |
| Motion | full, reduced crossfade, none | Motion is presentation only and never carries state or advances application data. |

The visual system projects one application-owned `AppearancePreferences`
snapshot into the pinned theme, `gpui-ai` sizing, and `gpui-ai` motion globals.
Do not create a second theme or animation lifecycle inside a feature.

## Component boundary

Use upstream components directly where their contract fits:

- `Chat` and `PromptBar` for conversation, selection, scrolling, and IME;
- `StreamingText`, `Thinking`, and `ToolCall` for controlled progressive state;
- `ApprovalCard`, `AttachmentStrip`, `MessageQueue`, and `ContextMeter` for
  typed agent surfaces; and
- `ThreadList`, `SidebarNav`, and `CommandSearch` for retained, virtualized,
  keyboard-operable collections.

Use `gpui-component` buttons, menus, dialogs, popovers, tooltips, inputs, and
layout primitives for ordinary application controls. Compose a Xana-specific
surface locally when its product meaning is unique. For a generally reusable
missing AI primitive, first record a minimal reproduction and decide whether
to contribute it to `gpui-ai`; do not copy upstream source into Xana.

Entity-backed components are created once and updated through their public
setters. Stateless components are rebuilt from bounded immutable snapshots.
Every subscription is retained for the lifetime of its owner. A constrained
view has one intentional overflow owner; do not nest competing scroll regions
around a component that already virtualizes its collection.

## Copy and localization

Client copy is selected from stable semantic message codes and typed bounded
parameters. Words never determine action identity, authority, confirmation
target, or receipt meaning. Unknown codes render a safe fallback containing
the original code. The current Spanish fixtures intentionally cover setup,
approval, and completion receipts; other Spanish messages identify their
fallback rather than pretending to be translated.

## Accessibility contract

- Every action is keyboard reachable and has a visible and accessible name.
- Focus order follows reading order and focus remains visible in every palette.
- Approval, failure, attention, controller, usage, and unavailable states do
  not depend on color, motion, hover, or pointer input alone.
- Selectable prose and code remain selectable; the retained `PromptBar` owns
  text editing and IME composition.
- 200% text uses wrapping and one bounded page scroll. Growing thread and
  navigation collections retain stable identity and upstream virtualization.
- Reduced/no-motion preferences affect decoration only; work state remains
  application-owned.

Automated tests cover palette contrast, bounded scale, semantic-code fallback,
expanded copy, stable fixture identity, and the pinned components' own focus,
keyboard, selection, IME, scaling, and reduced-motion contracts. Before layout
sign-off, the owner records this manual matrix on supported hosts:

| Check | Windows | macOS | Linux |
| --- | --- | --- | --- |
| Keyboard-only traversal and activation | Pending owner review | Pending owner review | Pending owner review |
| Screen reader (NVDA, VoiceOver, Orca/equivalent) | Pending owner review | Pending owner review | Pending owner review |
| 200% text and narrow-window reflow | Pending owner review | Pending owner review | Pending owner review |
| Light, dark, and high contrast | Pending owner review | Pending owner review | Pending owner review |
| IME composition and selectable content | Pending owner review | Pending owner review | Pending owner review |

Manual rows must not be marked complete from compilation or screenshots alone.
