#!/usr/bin/env python3
"""Offline regression tests for publication verification and upstream monitoring."""

import base64
from copy import deepcopy
from email.message import Message
import hashlib
from http.client import IncompleteRead
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import urllib.error
import urllib.request
import urllib.response

import automation_http as http

ROOT = Path(__file__).resolve().parents[1]


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


notes = load_script("release-notes")
release = load_script("verify-release")
monitor = load_script("check-codex-compatibility")
COMMIT = "a" * 40
HEAD = "b" * 40
TAG = "v0.4.0"
REPO = release.PRODUCTION_REPO


class ReleaseNotesTests(unittest.TestCase):
    def test_selects_exact_section_preserving_markdown(self):
        changelog = """## [Unreleased]
- Future work
## [0.4.0] - 2026-09-25
### Fixed
- Preserve **account identity**.

```markdown
## [0.4.0]
```
## [0.3.2]
- Older work
[0.4.0]: https://github.com/midhunmonachan/codex-profiles/compare/v0.3.2...v0.4.0
"""
        actual = notes.release_notes(changelog, TAG)
        self.assertIn("- Preserve **account identity**.", actual)
        self.assertIn("```markdown\n## [0.4.0]\n```", actual)
        self.assertIn("**Full changelog:** https://github.com/", actual)
        self.assertNotIn("Future work", actual)
        self.assertNotIn("Older work", actual)

    def test_rejects_missing_duplicate_empty_and_unclosed_sections(self):
        for text in (
            "## [Unreleased]\n- Future\n",
            "## [0.4.0]\n- One\n## [0.4.0]\n- Two\n",
            "## [0.4.0]\n### Added\n---\n<!-- pending review -->\n",
            "## [0.4.0]\n```\n- Unclosed\n",
            "## [0.4.0]\n- Item\n[0.4.0]: https://example.com/one\n[0.4.0]: https://example.com/two\n",
        ):
            with self.subTest(text=text), self.assertRaises(ValueError):
                notes.release_notes(text, TAG)

    def test_tag_checkout_reads_tagged_notes(self):
        with tempfile.TemporaryDirectory(prefix="codex-notes-test-") as temporary:
            directory = Path(temporary)

            def git(*args):
                subprocess.run(["git", *args], cwd=directory, check=True, capture_output=True)

            git("init", "--quiet")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (directory / "CHANGELOG.md").write_text("## [0.4.0]\n- Tagged notes.\n", encoding="utf-8")
            git("add", "CHANGELOG.md")
            git("-c", "commit.gpgsign=false", "commit", "-qm", "fixture")
            git("-c", "tag.gpgsign=false", "tag", TAG)
            (directory / "CHANGELOG.md").write_text("## [0.4.0]\n- Later edits.\n", encoding="utf-8")
            git("-c", "commit.gpgsign=false", "commit", "-am", "later", "--quiet")
            git("checkout", "--quiet", TAG)
            result = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/release-notes.py"), TAG],
                                    cwd=directory, check=True, capture_output=True, text=True)
            self.assertEqual(result.stdout, "- Tagged notes.\n")


class PublicHttpTests(unittest.TestCase):
    def test_retries_transient_status_without_credentials(self):
        opener = Mock()
        opener.open.side_effect = [urllib.error.HTTPError("", 503, "", {}, None), io.BytesIO(b"ok")]
        with patch.object(http.urllib.request, "build_opener", return_value=opener), \
                patch.object(http.time, "sleep") as sleep:
            self.assertEqual(http.fetch_bytes("https://api.github.com/example"), b"ok")
        self.assertEqual(opener.open.call_count, 2)
        self.assertEqual(sleep.call_args.args, (1,))
        request = opener.open.call_args.args[0]
        self.assertEqual(set(dict(request.header_items())), {"User-agent"})
        self.assertEqual(opener.open.call_args.kwargs["timeout"], 20)

    def test_retry_budget_and_protocol_failures(self):
        for error in (urllib.error.HTTPError("", 404, "", {}, None),
                      IncompleteRead(b"partial"), TimeoutError()):
            opener = Mock()
            opener.open.side_effect = error
            with self.subTest(error=type(error).__name__), \
                    patch.object(http.urllib.request, "build_opener", return_value=opener), \
                    patch.object(http.time, "sleep") as sleep, self.assertRaises(ValueError):
                http.fetch_bytes("https://api.github.com/example")
            self.assertEqual(opener.open.call_count, 4)
            self.assertEqual(sleep.call_count, 3)

    def test_retry_after_is_respected_or_rejected(self):
        for delay in ("5", "16", "tomorrow"):
            opener = Mock()
            opener.open.side_effect = [urllib.error.HTTPError("", 429, "", {"Retry-After": delay}, None),
                                      io.BytesIO(b"ok")]
            with self.subTest(delay=delay), \
                    patch.object(http.urllib.request, "build_opener", return_value=opener), \
                    patch.object(http.time, "sleep") as sleep:
                if delay == "5":
                    self.assertEqual(http.fetch_bytes("https://api.github.com/example"), b"ok")
                    sleep.assert_called_once_with(5)
                else:
                    with self.assertRaises(ValueError):
                        http.fetch_bytes("https://api.github.com/example")
                    sleep.assert_not_called()

    def test_public_urls_and_redirects_reject_other_destinations(self):
        request = urllib.request.Request("https://api.github.com/example")
        for url in ("https://evil.example/", "http://api.github.com/", "https://user@api.github.com/",
                    "https://api.github.com:444/", "https://api.github.com.evil.example/"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                http.PublicRedirect().redirect_request(request, None, 302, "", {}, url)
        redirected = http.PublicRedirect().redirect_request(
            request, None, 302, "", {}, "https://static.crates.io/example")
        self.assertEqual(redirected.full_url, "https://static.crates.io/example")

    def test_fetch_enforces_redirect_policy_before_opening_destination(self):
        opened = []
        bodies = []

        class FixtureHttps(urllib.request.HTTPSHandler):
            def https_open(self, request):
                opened.append(request.full_url)
                headers = Message()
                headers["Location"] = "https://evil.example/payload"
                code = 302 if len(opened) == 1 else 200
                body = io.BytesIO(b"payload")
                bodies.append(body)
                response = urllib.response.addinfourl(body, headers, request.full_url, code)
                response.msg = "Fixture"
                return response

        build_opener = urllib.request.build_opener
        with patch.object(http.urllib.request, "build_opener",
                          side_effect=lambda *handlers: build_opener(*handlers, FixtureHttps())), \
                self.assertRaisesRegex(ValueError, "Unexpected public metadata URL"):
            http.fetch_bytes("https://api.github.com/fixture")
        self.assertEqual(opened, ["https://api.github.com/fixture"])
        self.assertTrue(all(body.closed for body in bodies))

    def test_response_size_limit(self):
        opener = Mock()
        opener.open.return_value = io.BytesIO(b"12345")
        with patch.object(http.urllib.request, "build_opener", return_value=opener), \
                self.assertRaisesRegex(ValueError, "size limit"):
            http.fetch_bytes("https://api.github.com/example", limit=4)


class PublishedReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="codex-release-test-")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        payloads = release.expected_assets("0.4.0")
        for name in payloads:
            (self.directory / name).write_bytes(f"synthetic {name}".encode())
        self.hashes = {name: hashlib.sha256((self.directory / name).read_bytes()).hexdigest()
                       for name in payloads}
        (self.directory / "SHA256SUMS").write_text(
            "".join(f"{digest}  {name}\n" for name, digest in self.hashes.items()), encoding="utf-8")
        self.manifest = {
            "version": "0.4.0", "tag": TAG, "commit": COMMIT,
            "repository": {"slug": REPO, "url": f"https://github.com/{REPO}"},
            "artifacts": [{"path": name, "sha256": self.hashes[name], "category": category}
                          for name, category in payloads.items()],
        }
        self.write_manifest()
        self.metadata = {"tag_name": TAG, "draft": False, "prerelease": False, "immutable": True}
        self.refresh_asset_metadata()

    def write_manifest(self):
        (self.directory / "release-manifest.json").write_text(json.dumps(self.manifest), encoding="utf-8")

    def refresh_asset_metadata(self):
        self.metadata["assets"] = [
            {"name": path.name, "size": path.stat().st_size,
             "digest": "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()}
            for path in self.directory.iterdir()
        ]

    def verify(self):
        release.verify_assets(self.directory, self.metadata, REPO, TAG, COMMIT)

    def test_complete_release_and_duplicate_or_missing_assets(self):
        self.verify()
        self.assertEqual(len(self.metadata["assets"]), 15)
        original = deepcopy(self.metadata)
        for assets in (original["assets"][:-1], original["assets"] + [original["assets"][0]]):
            self.metadata["assets"] = assets
            with self.assertRaises(ValueError):
                self.verify()

    def test_corrupt_payload_is_detected_even_when_github_digest_matches(self):
        (self.directory / "codex-profiles.rb").write_bytes(b"changed")
        self.refresh_asset_metadata()
        with self.assertRaisesRegex(ValueError, "Checksums"):
            self.verify()

    def test_manifest_binds_commit_repository_category_and_membership(self):
        original = deepcopy(self.manifest)
        mutations = (
            lambda m: m.update(commit=HEAD),
            lambda m: m["repository"].update(slug="someone/else"),
            lambda m: m["artifacts"][0].update(category="cargo"),
            lambda m: m["artifacts"].append(m["artifacts"][0]),
        )
        for mutate in mutations:
            self.manifest = deepcopy(original)
            mutate(self.manifest)
            self.write_manifest()
            self.refresh_asset_metadata()
            with self.assertRaises(ValueError):
                self.verify()

    def test_duplicate_checksums_are_rejected(self):
        path = self.directory / "SHA256SUMS"
        path.write_text(path.read_text() + path.read_text().splitlines()[0] + "\n", encoding="utf-8")
        self.refresh_asset_metadata()
        with self.assertRaisesRegex(ValueError, "Duplicate checksum"):
            self.verify()

    def test_registry_bytes_and_integrity(self):
        def metadata(url):
            if url.startswith("https://crates.io/"):
                return {"version": {"crate": "codex-profiles", "num": "0.4.0", "yanked": False,
                                    "checksum": self.hashes["codex-profiles-0.4.0.crate"]}}
            name = url.split("/")[-2]
            data = (self.directory / f"{name}-0.4.0.tgz").read_bytes()
            return {"name": name, "version": "0.4.0", "dist": {
                "integrity": "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode(),
                "attestations": {"provenance": {"predicateType": "https://slsa.dev/provenance/v1"}},
            }}

        def tarball(url):
            return (self.directory / url.rsplit("/", 1)[1]).read_bytes()

        with patch.object(release, "fetch_json", side_effect=metadata) as fetch_metadata, \
                patch.object(release, "fetch_bytes", side_effect=tarball) as fetch_tarball, \
                patch("sys.stdout", new_callable=io.StringIO):
            release.verify_registries(self.directory, REPO, TAG)
            self.assertEqual(fetch_metadata.call_count, 7)
            self.assertEqual(fetch_tarball.call_count, 7)
            fetch_tarball.side_effect = lambda url: b"wrong" if url.endswith(".crate") else tarball(url)
            with self.assertRaisesRegex(ValueError, "Registry crate differs"):
                release.verify_registries(self.directory, REPO, TAG)
            fetch_tarball.side_effect = lambda url: b"wrong"
            with self.assertRaisesRegex(ValueError, "npm tarball differs"):
                release.verify_registries(self.directory, REPO, TAG)
            fetch_metadata.side_effect = lambda url: {
                "name": "codex-profiles", "version": "0.4.0", "dist": {"integrity": "sha512-wrong"}}
            with self.assertRaisesRegex(ValueError, "npm integrity"):
                release.verify_registries(self.directory, REPO, TAG)

    def test_prereleases_and_forks_do_not_query_registries(self):
        with patch.object(release, "fetch_json") as request, \
                patch.object(release, "fetch_bytes") as download, patch("sys.stdout", new_callable=io.StringIO):
            release.verify_registries(self.directory, REPO, "v0.4.0-beta.1")
            release.verify_registries(self.directory, "fixture/fork", TAG)
            request.assert_not_called()
            download.assert_not_called()

    def test_attestations_require_exact_producer_identity(self):
        with patch.object(release, "gh") as gh, patch("sys.stdout", new_callable=io.StringIO):
            release.verify_attestations(self.directory, REPO, TAG, COMMIT)
        self.assertEqual(gh.call_count, 16)
        paths = [Path(call.args[2]).name for call in gh.call_args_list[1:]]
        self.assertEqual(len(paths), len(set(paths)))
        self.assertEqual(set(paths), {path.name for path in self.directory.iterdir()})
        for call in gh.call_args_list[1:]:
            args = call.args
            self.assertEqual(args[args.index("--source-ref") + 1], f"refs/tags/{TAG}")
            self.assertEqual(args[args.index("--source-digest") + 1], COMMIT)
            self.assertEqual(args[args.index("--signer-workflow") + 1], f"{REPO}/.github/workflows/release.yml")
            self.assertIn("--deny-self-hosted-runners", args)
        with patch.object(release, "gh", side_effect=ValueError("Attestation rejected")), \
                self.assertRaisesRegex(ValueError, "Attestation rejected"):
            release.verify_attestations(self.directory, REPO, TAG, COMMIT)

    def test_tag_resolution_dereferences_annotated_tags(self):
        with patch.object(release, "github_json", side_effect=[
            {"object": {"type": "tag", "sha": HEAD}}, {"object": {"type": "commit", "sha": COMMIT}},
        ]) as request:
            self.assertEqual(release.resolve_tag(REPO, TAG), COMMIT)
        self.assertIn(f"git/tags/{HEAD}", request.call_args.args[0])

    def test_wrong_commit_and_oversized_assets_stop_before_download(self):
        with patch.object(release, "resolve_tag", return_value=COMMIT), \
                patch.object(release, "github_json", return_value=self.metadata), \
                patch.object(release, "gh") as gh:
            with self.assertRaisesRegex(ValueError, "commit mismatch"):
                release.verify_release(REPO, TAG, HEAD)
            self.metadata["assets"][0]["size"] = release.MAX_ASSET_BYTES + 1
            with self.assertRaisesRegex(ValueError, "size exceeds"):
                release.verify_release(REPO, TAG, COMMIT)
            gh.assert_not_called()

    def test_download_directory_is_removed_after_failure(self):
        downloaded = []

        def fail_download(*args):
            downloaded.append(Path(args[args.index("--dir") + 1]))
            raise ValueError("Download failed")

        with patch.object(release, "resolve_tag", return_value=COMMIT), \
                patch.object(release, "github_json", return_value=self.metadata), \
                patch.object(release, "gh", side_effect=fail_download), \
                self.assertRaisesRegex(ValueError, "Download failed"):
            release.verify_release(REPO, TAG, COMMIT)
        self.assertEqual(len(downloaded), 1)
        self.assertFalse(downloaded[0].exists())


class CompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.baseline = json.loads(monitor.DEFAULT_BASELINE.read_text(encoding="utf-8"))
        self.tree = {"truncated": False, "tree": [
            {"path": path, "type": "blob", "mode": "100644", "sha": COMMIT}
            for path in self.baseline["paths"]
        ]}

    def check(self, tree):
        with patch.object(monitor, "fetch_json", side_effect=[{"sha": HEAD}, self.tree, tree]) as request:
            report = monitor.check_compatibility(self.baseline)
        self.assertEqual([call.args[0] for call in request.call_args_list], [
            f"{monitor.API}/commits/main",
            f"{monitor.API}/git/trees/{self.baseline['reviewed_commit']}?recursive=1",
            f"{monitor.API}/git/trees/{HEAD}?recursive=1",
        ])
        return report

    def test_unchanged_and_unrelated_files(self):
        after = deepcopy(self.tree)
        after["tree"].extend({"path": f"unrelated/{i}"} for i in range(350))
        self.assertEqual(self.check(after)["status"], "unchanged")
        after["tree"][0]["sha"] = HEAD
        self.assertEqual(self.check(after)["status"], "review_required")

    def test_removed_changed_mode_and_changed_type_require_review(self):
        for field, value in (("sha", HEAD), ("mode", "100755"), ("type", "tree"), (None, None)):
            after = deepcopy(self.tree)
            if field:
                after["tree"][0][field] = value
            else:
                after["tree"].pop(0)
            with self.subTest(field=field):
                report = self.check(after)
                self.assertEqual(report["status"], "review_required")
                self.assertNotEqual(report["paths"][0]["status"], "unchanged")

    def test_missing_baseline_and_truncated_or_malformed_trees_fail(self):
        malformed = ({"truncated": True, "tree": []}, {"tree": []},
                     {"truncated": False, "tree": {}}, {"truncated": False, "tree": [None]})
        for tree in malformed:
            with self.subTest(tree=tree), self.assertRaises(ValueError):
                self.check(tree)
        self.tree["tree"].pop(0)
        with self.assertRaisesRegex(ValueError, "Reviewed path"):
            self.check(deepcopy(self.tree))

    def test_invalid_baseline_never_queries_network(self):
        for key, value in (("repository", "someone/else"), ("branch", "other"),
                           ("reviewed_commit", "main"), ("paths", ["../auth.json"]),
                           ("paths", ["a", "a"])):
            baseline = {**self.baseline, key: value}
            with self.subTest(key=key), patch.object(monitor, "fetch_json") as request, \
                    self.assertRaises(ValueError):
                monitor.check_compatibility(baseline)
            request.assert_not_called()

    def test_protocol_failure_produces_incomplete_report_and_failure_exit(self):
        opener = Mock()
        opener.open.side_effect = IncompleteRead(b"partial")
        with tempfile.TemporaryDirectory(prefix="codex-monitor-test-") as temporary, \
                patch.object(http.urllib.request, "build_opener", return_value=opener), \
                patch.object(http.time, "sleep"), patch("sys.stdout", new_callable=io.StringIO), \
                patch.object(sys, "argv", ["monitor", "--output-dir", temporary]):
            self.assertEqual(monitor.main(), 1)
            report = json.loads((Path(temporary) / "report.json").read_text())
            self.assertEqual(report["status"], "incomplete")
            self.assertIn("no compatibility conclusion", (Path(temporary) / "report.md").read_text())

    def test_documented_baseline_and_no_mutation(self):
        original = monitor.DEFAULT_BASELINE.read_bytes()
        docs = (ROOT / "docs/compatibility.md").read_text(encoding="utf-8")
        self.assertIn(self.baseline["reviewed_commit"], docs)
        for path in self.baseline["paths"]:
            self.assertIn(f"`{path}`", docs)
        self.check(deepcopy(self.tree))
        self.assertEqual(monitor.DEFAULT_BASELINE.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
