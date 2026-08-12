import json
import subprocess


def test_manifest_reports_name_version_and_capabilities(act_command, wasm_path):
    out = subprocess.run(
        [*act_command, "inspect", "component-manifest", str(wasm_path)],
        capture_output=True, text=True, check=True,
    ).stdout
    manifest = json.loads(out)
    assert manifest["std"]["name"] == "servo"
    assert isinstance(manifest["std"]["version"], str)
    capabilities = manifest["std"]["capabilities"]
    assert "wasi:sockets" in capabilities
    assert "wasi:filesystem" in capabilities
