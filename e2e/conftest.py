"""Shared fixtures for the MCP-driven e2e suite.

The suite drives the packed component through `act run --mcp` over stdio with
a real MCP client, so what the tests observe is what an agent observes.
"""

import json
import os
import shlex
import subprocess
import pytest
from pathlib import Path

from fastmcp import Client
from fastmcp.client.transports import StdioTransport

WASM = "target/wasm32-wasip2/release/component_servo.wasm"

# ACT's audit trail writes to stderr unconditionally — it is not governed by
# RUST_LOG — so it is redirected to a file rather than left to flood pytest.
LOG_FILE = Path(".pytest-act-stderr.log")

# The engine keeps client storage (localStorage and friends) under /tmp/servo
# and refuses to start without a writable directory there — the path is the
# engine's own, not this component's, so the grant below names it verbatim.
CLIENT_STORAGE = Path("/tmp/servo")


@pytest.fixture(scope="session")
def act_command() -> list[str]:
    """The ACT invocation, honouring the same override the justfile uses.

    Parsed with shlex, not treated as a single path: the justfile's own
    default for its `act` variable is `npx @actcore/act` — two words — which
    cannot be `argv[0]` for a non-shell `subprocess.run`/`StdioTransport`
    call. A bare `os.environ.get("ACT", "act")` string breaks that default;
    splitting it is what makes both forms ("act" on PATH, and the npx
    two-word default) actually spawn.
    """
    return shlex.split(os.environ.get("ACT", "act"))


@pytest.fixture(scope="session")
def wasm_path(act_command: list[str]) -> Path:
    """The packed component.

    Existence is not enough and neither is a fresh mtime: `cargo build`
    produces a wasm with no `act:component` custom section, and an unpacked
    artifact declares no capability ceiling, so every grant is refused as
    "outside ceiling" and the failures point anywhere but here. This has
    already bitten this workspace repeatedly, so the fixture checks the
    section rather than the file.

    Unlike every other component in this sweep, a missing/stale wasm here is
    not a quick `cargo build` away: this engine compiles SpiderMonkey,
    FreeType, aws-lc-rs and swgl along the way, and a cold build is measured
    in tens of minutes (see the CI comment in .github/workflows/ci.yml). The
    failure message says so explicitly rather than just naming the recipe.
    """
    path = Path(WASM)
    if not path.exists():
        pytest.fail(
            f"{path} is missing — run `just build && just pack` first "
            "(a cold build of this component takes ~20 minutes; it compiles "
            "the Servo engine itself, not just this crate)"
        )
    probe = subprocess.run(
        [*act_command, "inspect", "component-manifest", str(path)],
        capture_output=True, text=True,
    )
    name = json.loads(probe.stdout or "{}").get("std", {}).get("name", "unknown")
    if name in ("", "unknown"):
        pytest.fail(f"{path} is built but not packed — run `just pack`")
    return path


@pytest.fixture
async def client(act_command: list[str], wasm_path: Path):
    """A connected MCP client, one `act` process per test.

    The `wasi:filesystem` grant moves here verbatim from the old justfile's
    `act run ... --grant '{"wasi:filesystem":{"mode":"allowlist","allow":
    [{"path":"/tmp/servo/**","mode":"rw"}]}}'` — nothing in `wasi:sockets`
    (the other declared capability) is granted, matching the old recipe: none
    of the three hurl files ever passed a `url`, only `html`, so no test here
    needs outbound network access.
    """
    CLIENT_STORAGE.mkdir(parents=True, exist_ok=True)
    grant = json.dumps({
        "wasi:filesystem": {
            "mode": "allowlist",
            "allow": [{"path": f"{CLIENT_STORAGE}/**", "mode": "rw"}],
        }
    })
    transport = StdioTransport(
        command=act_command[0],
        args=[*act_command[1:], "run", str(wasm_path), "--mcp", "--grant", grant],
        keep_alive=False,
        log_file=LOG_FILE,
    )
    async with Client(transport) as connected:
        yield connected
