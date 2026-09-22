"""Real terminal regressions; every credential and home directory is synthetic."""

import base64
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import time


BINARY = sys.argv[1]
DOWN = b"\x1b[B"


def exit_code(status):
    """Convert waitpid status without requiring Python 3.9's helper."""
    converter = getattr(os, "waitstatus_to_exitcode", None)
    if converter is not None:
        return converter(status)
    if os.WIFEXITED(status):
        return os.WEXITSTATUS(status)
    if os.WIFSIGNALED(status):
        return 128 + os.WTERMSIG(status)
    return 1


def credentials(account):
    payload = {
        "email": f"{account}@example.com",
        "https://api.openai.com/auth": {"chatgpt_plan_type": "plus"},
    }
    encoded = base64.urlsafe_b64encode(json.dumps(payload).encode()).rstrip(b"=").decode()
    return {"tokens": {
        "account_id": account,
        "id_token": f"e30.{encoded}.",
        "access_token": "synthetic-access",
        "refresh_token": "synthetic-refresh",
    }}


def run_terminal(root, args, interactions):
    environment = dict(os.environ, HOME=str(root), CODEX_HOME=str(root),
                       CODEX_PROFILES_SKIP_UPDATE="1", NO_COLOR="1", TERM="xterm-256color")
    pid, terminal = pty.fork()
    if pid == 0:
        os.execve(BINARY, [BINARY, "--plain", *args], environment)
    fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))
    output = bytearray()
    pending = bytearray()
    remaining = list(interactions)
    deadline = time.monotonic() + 15
    status = None
    reaped = False
    try:
        while time.monotonic() < deadline:
            if select.select([terminal], [], [], 0.05)[0]:
                try:
                    chunk = os.read(terminal, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                output.extend(chunk)
                pending.extend(chunk)
                if b"\x1b[6n" in pending:
                    os.write(terminal, b"\x1b[1;1R")
                    pending = pending.replace(b"\x1b[6n", b"")
                if remaining and remaining[0][0].encode() in pending:
                    _, keys = remaining.pop(0)
                    if callable(keys):
                        keys = keys()
                    os.write(terminal, keys)
                    pending.clear()
            waited, status = os.waitpid(pid, os.WNOHANG)
            if waited:
                reaped = True
                break
        else:
            raise AssertionError(f"terminal timed out: {args}: {output!r}")
        if not reaped:
            _, status = os.waitpid(pid, 0)
            reaped = True
        assert not remaining, f"prompts not reached: {remaining}: {output!r}"
        return exit_code(status), output.decode(errors="replace")
    finally:
        os.close(terminal)
        if not reaped:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)


def setup(root, active="unsaved"):
    profiles = root / "profiles"
    profiles.mkdir()
    for account in ["alpha", "beta"]:
        (profiles / f"{account}.json").write_text(json.dumps(credentials(account)))
    (root / "auth.json").write_text(json.dumps(credentials(active)))


def active_account(root):
    return json.loads((root / "auth.json").read_text())["tokens"]["account_id"]


def scenario(name, run):
    with tempfile.TemporaryDirectory(prefix="codex-profiles-terminal-") as directory:
        root = Path(directory)
        setup(root)
        run(root)
        print(f"passed: {name}")


def unsaved(root, keys, expected, saved=False):
    code, output = run_terminal(root, ["load", "--id", "alpha"],
                                [("Continue without saving", keys)])
    assert active_account(root) == expected, output
    assert code == 0, output
    if expected != "alpha":
        assert "Cancelled" in output, output
    assert (root / "profiles" / "unsaved@example.com-plus.json").exists() == saved


scenario("save before switching", lambda root: unsaved(root, b"\r", "alpha", True))
scenario("continue without saving", lambda root: unsaved(root, DOWN + b"\r", "alpha"))
scenario("cancel unsaved switch", lambda root: unsaved(root, DOWN + DOWN + b"\r", "unsaved"))
scenario("escape unsaved switch", lambda root: unsaved(root, b"\x1b", "unsaved"))


def load_picker(root):
    (root / "auth.json").write_text(json.dumps(credentials("alpha")))
    code, output = run_terminal(root, ["load"], [("ENTER to load", DOWN + b"\r")])
    assert code == 0 and active_account(root) == "beta", output


scenario("load from terminal picker", load_picker)


def load_rejects_active_auth_change(root):
    (root / "auth.json").write_text(json.dumps(credentials("alpha")))

    def mutate_active_auth():
        (root / "auth.json").write_text(json.dumps(credentials("changed")))
        return DOWN + b"\r"

    code, output = run_terminal(
        root,
        ["load"],
        [("ENTER to load", mutate_active_auth)],
    )
    assert code != 0 and "changed on disk" in output.lower(), output
    assert active_account(root) == "changed", output


scenario("load preserves active auth changed during picker", load_rejects_active_auth_change)


def delete_one(root, answer):
    code, output = run_terminal(root, ["delete", "--id", "alpha"],
                                [("This cannot be undone.", answer + b"\r")])
    assert (root / "profiles" / "alpha.json").exists() == (answer != b"y"), output
    assert code == 0, output


scenario("confirm deletion", lambda root: delete_one(root, b"y"))
scenario("decline deletion", lambda root: delete_one(root, b"n"))


def delete_many(root):
    code, output = run_terminal(root, ["delete"], [
        ("SPACE", b" " + DOWN + b" " + b"\r"),
        ("Delete selected profiles?", b"y\r"),
    ])
    assert code == 0, output
    assert not (root / "profiles" / "alpha.json").exists()
    assert not (root / "profiles" / "beta.json").exists()


scenario("select and delete several profiles", delete_many)


def empty_selection(root):
    code, output = run_terminal(root, ["delete"], [("SPACE", b"\r")])
    assert code == 0 and "Cancelled" in output, output
    assert (root / "profiles" / "alpha.json").exists()


scenario("empty selection cancels deletion", empty_selection)


def deleted_during_confirmation(root):
    def remove_selected():
        (root / "profiles" / "alpha.json").unlink()
        return b"y\r"

    code, output = run_terminal(root, ["delete", "--id", "alpha"],
                                [("This cannot be undone.", remove_selected)])
    assert code == 1 and "not found" in output.lower(), output
    assert (root / "profiles" / "beta.json").exists()


scenario("selected file disappears during confirmation", deleted_during_confirmation)


def permission_changes_during_confirmation(root):
    def remove_write_permission():
        (root / "profiles").chmod(0o500)
        return b"y\r"

    try:
        code, output = run_terminal(root, ["delete", "--id", "alpha"],
                                    [("This cannot be undone.", remove_write_permission)])
        assert code == 1 and "delete" in output.lower(), output
        assert (root / "profiles" / "alpha.json").exists()
    finally:
        (root / "profiles").chmod(0o700)


# Root bypasses POSIX mode bits, so the filesystem cannot reproduce this failure.
if os.geteuid() != 0:
    scenario("selected file becomes undeletable", permission_changes_during_confirmation)
