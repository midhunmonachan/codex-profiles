"""Bounded, credential-free reads of public release and upstream metadata."""

import http.client
import json
import time
import urllib.error
import urllib.parse
import urllib.request


HOSTS = {"api.github.com", "registry.npmjs.org", "crates.io", "static.crates.io"}


def validate_url(url):
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != "https" or parsed.hostname not in HOSTS
            or parsed.username or parsed.password or parsed.port not in (None, 443)):
        raise ValueError("Unexpected public metadata URL")


class PublicRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        try:
            validate_url(new_url)
        except ValueError:
            if fp is not None:
                fp.close()
            raise
        return super().redirect_request(request, fp, code, message, headers, new_url)


def fetch_bytes(url, *, limit=64 * 1024 * 1024):
    validate_url(url)
    request = urllib.request.Request(url, headers={"User-Agent": "codex-profiles-verification"})
    opener = urllib.request.build_opener(PublicRedirect())
    for attempt in range(4):
        delay = 2 ** attempt
        try:
            with opener.open(request, timeout=20) as response:
                data = response.read(limit + 1)
            if len(data) > limit:
                raise ValueError("Public response exceeds size limit")
            return data
        except urllib.error.HTTPError as error:
            retry_after = (error.headers or {}).get("Retry-After")
            error.close()
            if error.code not in (404, 429, 500, 502, 503, 504) or attempt == 3:
                raise ValueError(f"Public request failed with HTTP {error.code}") from None
            if retry_after is not None:
                if not retry_after.isdigit() or int(retry_after) > 15:
                    raise ValueError("Retry-After exceeds the verification retry budget") from None
                delay = max(delay, int(retry_after))
        except (urllib.error.URLError, TimeoutError, http.client.HTTPException):
            if attempt == 3:
                raise ValueError("Public request failed after four attempts") from None
        time.sleep(delay)
    raise ValueError("Public request did not complete")


def fetch_json(url):
    return json.loads(fetch_bytes(url, limit=8 * 1024 * 1024))
