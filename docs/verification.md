# Release Verification

Each GitHub release includes:

- `SHA256SUMS`
- `release-manifest.json`
- GitHub artifact attestations for release assets
- npm provenance for published npm packages

## Verify GitHub release assets

Download the release asset you want to inspect together with `SHA256SUMS` and
`release-manifest.json`:

```bash
TAG="vX.Y.Z"
ASSET="codex-profiles-x86_64-unknown-linux-gnu.tar.gz"
gh release download "$TAG" \
  --repo midhunmonachan/codex-profiles \
  --pattern 'SHA256SUMS' \
  --pattern 'release-manifest.json' \
  --pattern "$ASSET"
```

Replace `vX.Y.Z` with the release tag you want to verify.

Select the checksum for that asset and verify it:

```bash
awk -v asset="$ASSET" '$2 == asset { print; found++ } END { if (found != 1) exit 1 }' \
  SHA256SUMS > selected-SHA256SUMS &&
shasum -a 256 -c selected-SHA256SUMS
```

On systems with GNU coreutils:

```bash
sha256sum -c selected-SHA256SUMS
```

Use the complete `SHA256SUMS` with `-c` only when all listed artifacts have been
downloaded; otherwise the checker also reports missing files for other platforms.

`release-manifest.json` records the release version, tag, commit SHA, tool
versions, and the same per-asset hashes from `SHA256SUMS`.

The release workflow also checks that each archive contains the expected
executable bytes, npm packages declare the matching platform metadata and
binary layout, and the Homebrew cask points to the matching Darwin archives.

## Verify GitHub attestations

Use the GitHub CLI to verify a release asset attestation:

```bash
gh attestation verify codex-profiles-x86_64-unknown-linux-gnu.tar.gz \
  -R midhunmonachan/codex-profiles
```

Replace the asset name with the file you downloaded from the release.

## npm packages

npm packages are published with trusted publishing and provenance.

The matching npm tarballs are also uploaded to the GitHub release, so you can:

- verify their hashes with `SHA256SUMS`
- inspect them in `release-manifest.json`
- verify the GitHub release attestations for the uploaded tarballs

## crates.io package

The `.crate` published for crates.io is also uploaded to the GitHub release.
You can verify it the same way:

- compare its hash against `SHA256SUMS`
- confirm it appears in `release-manifest.json`
- verify the GitHub release attestation for the `.crate` asset
