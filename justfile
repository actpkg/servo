wasm := "target/wasm32-wasip2/release/component_servo.wasm"
# OCI reference to publish to (registry/namespace/name, no tag). Override with OCI_REF.
component_ref := env("OCI_REF", "actpkg.dev/library/servo")

act := env("ACT", "npx @actcore/act")
actbuild := env("ACT_BUILD", "npx @actcore/act-build")

# Check the one thing this component cannot build without. The engine itself is a
# git dependency, so cargo fetches it.
init:
    #!/usr/bin/env bash
    set -euo pipefail
    test -d /opt/wasi-sdk || {
        echo "wasi-sdk not found at /opt/wasi-sdk." >&2
        echo "The engine builds C along the way: SpiderMonkey, FreeType," >&2
        echo "aws-lc-rs, swgl. Get it from" >&2
        echo "https://github.com/WebAssembly/wasi-sdk/releases" >&2
        exit 1
    }

# Build and pack. Packing is part of building on purpose: `cargo build` alone
# produces a wasm with no `act:component` section, which declares no capability
# ceiling, so at runtime every grant is refused as "outside ceiling" and the
# failure points anywhere but at the missing metadata.
build: init
    cargo build --target wasm32-wasip2 --release
    {{actbuild}} pack {{wasm}}

# Re-embed act:component metadata and act:skill without rebuilding. `pack` is
# idempotent, so running it after `build` is harmless.
pack:
    {{actbuild}} pack {{wasm}}

test: build
    ACT="{{act}}" uv run --project e2e pytest e2e/ -v

publish: build
    #!/usr/bin/env bash
    set -euo pipefail
    INFO=$({{act}} inspect component-manifest {{wasm}})
    VERSION=$(echo "$INFO" | jq -r .std.version)
    {{actbuild}} push {{wasm}} "{{component_ref}}:$VERSION" --skip-if-exists --also-tag latest
