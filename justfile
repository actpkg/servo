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
    # Serial on purpose. Each test drives its own `act run`, and this component
    # is the fleet's heavyweight: ~3 GB peak RSS per instance, against a
    # 120 MB module whose Cranelift compile cache is cold on a fresh CI
    # runner. In parallel, cargo's default, the overlapping compiles and
    # instantiations killed two of three children on the runner (silent
    # deaths — inspect and the handshake both lost their child mid-flight)
    # while passing everywhere with a warm cache. One at a time is what the
    # old python suite did, and it also removes any concurrent-`npx` install
    # race on a cold npm cache.
    cd e2e && ACT="{{act}}" WASM="../{{wasm}}" cargo test -- --test-threads=1

publish: build
    #!/usr/bin/env bash
    set -euo pipefail
    INFO=$({{act}} inspect component-manifest {{wasm}})
    VERSION=$(echo "$INFO" | jq -r .std.version)
    {{actbuild}} push {{wasm}} "{{component_ref}}:$VERSION" --skip-if-exists --also-tag latest
