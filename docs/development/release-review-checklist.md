# Release Preview draft review

> Audience: The Xana release owner reviewing an unpublished GitHub draft.

Do not publish a draft whose title still begins with `INCOMPLETE`. For a
`REVIEW READY` draft, verify all of the following against the pushed tag and
workflow run:

- the tag is exact `vX.Y.Z`, points to the reviewed commit, and matches every
  Cargo package, both manifests, the release title, and release notes;
- the draft contains exactly fifteen assets: four native archives, four
  archive sidecars, two manifests, two reviewed installers, release notes, this
  checklist, and `sha256.sum`;
- every archive hash matches its sidecar and `sha256.sum`, and GitHub
  attestations verify for every downloadable asset against `labcoder/xana` and
  the expected workflow commit;
- the exact tagged commit has a successful three-platform `main` CI push run
  covering formatting, Clippy, feature matrices, installers, and packaging;
- the Windows x64, macOS ARM64, macOS Intel, and Linux x64 glibc release jobs
  each rebuilt natively and passed archive inventory and version/help checks;
- the release notes state that the preview is unsigned, not Apple-notarized,
  and not Windows-Authenticode-signed, without recommending security bypasses;
- latest and exact-version installer URLs resolve inside this same draft, and a
  clean per-platform smoke preserves setup, PATH, update, and removal policy;
- no provider credentials, Xana config, sessions, artifacts, local owner paths,
  or unreviewed files appear in logs or assets.

Publishing remains a separate owner action after this review. Never mutate a
published asset or tag; make any correction as a new patch release.
