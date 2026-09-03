# M4 Desktop visual-system evidence

> Recorded: 2026-09-02
>
> Scope: M4-14 automated implementation evidence
>
> Manual accessibility and visual review: pending owner verification

## Delivered

- Xana-owned light, dark, and high-contrast semantic palettes with tested
  foreground/background contrast.
- Compact and comfortable control density, full/reduced/no motion, and bounded
  100–200% text scaling projected into the pinned GPUI component stack.
- Stable typed semantic copy codes, bounded parameters, safe unknown-code and
  missing-translation fallbacks, expanded pseudolocale fixtures, and
  representative Spanish setup/approval/receipt copy.
- A provider-free `xana-desktop --catalog` surface using real retained
  `Chat`, `PromptBar`, `ThreadList`, `SidebarNav`, and `CommandSearch` entities
  plus bounded progressive agent, approval, attachment, queue, and usage
  snapshots.
- A reviewed component inventory that states where `gpui-ai`,
  `gpui-component`, Xana composition, and later Workbench work own behavior.

## Verification

The following checks passed from the repository root on Windows:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
pwsh ./scripts/check-desktop-dependencies.ps1
```

Both workspace test modes passed 1,013 root library tests with 6 intentional
manual/timing ignores, 20 CLI integration tests, 4 settings integration tests,
and 19 Desktop tests. The dependency check confirmed that CLI/TUI remains free
of GPUI dependencies and Desktop resolves one reviewed GPUI source family.

`cargo run -p xana-desktop -- --catalog` compiled and opened a live native
window without resolving configuration or a provider. It remained active until
the smoke-test process was explicitly terminated.

## Human evidence still required

Compilation and an automated smoke launch do not establish platform
accessibility or visual quality. The permanent matrix in
`docs/contributing/desktop-visual-system.md` remains pending for keyboard-only,
NVDA/VoiceOver/Orca-equivalent, 200% reflow, theme/contrast, IME, and selectable
content checks. Final Workbench composition is also outside M4-14.
