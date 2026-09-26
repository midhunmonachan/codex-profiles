# Release Verification

Each GitHub release includes:

- `SHA256SUMS`
- `release-manifest.json`
- GitHub artifact attestations for release assets
- npm provenance for published npm packages

## Automated verification

The release workflow verifies the published result after uploading all assets and
publishing the GitHub release. An unsuccessful verification fails the release run;
it does not roll back packages already published to registries.

From a checkout containing the verifier, check an existing release without
publishing it again:

```bash
python3 -B scripts/verify-release.py v0.4.0
```

This requires Python 3.11+, an authenticated GitHub CLI with attestation support,
and network access. Add `--expected-commit FULL_SHA` to require a particular source
commit. Or run the read-only GitHub Actions workflow:

```bash
gh workflow run verify-release.yml --ref main -f tag=v0.4.0
```

The verifier requires an immutable release with exactly 15 expected assets. It
checks GitHub asset digests, every payload checksum, manifest membership and the
resolved tag commit, release integrity, and all asset attestations. Attestations
must come from this repository's release workflow at that tag and commit, using
GitHub-hosted runners. Assets larger than 64 MiB are rejected.

For stable production releases, it also compares all six public npm tarballs and
the crates.io package byte-for-byte with the attested release assets and checks
their registry integrity metadata. It checks that npm advertises provenance;
this is separate from verifying npm's provenance signatures. Prereleases and
forks skip registry verification because their publication workflow skips those
registries. Public registry requests use no credentials, allow at most four
attempts, and honor short `Retry-After` delays. Each socket operation has a
20-second timeout; the workflow has a 15-minute overall limit. Temporary
downloads are removed when verification finishes or raises an error.

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
