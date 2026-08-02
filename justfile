wasm := "target/wasm32-wasip2/release/component_servo.wasm"
# OCI reference to publish to (registry/namespace/name, no tag). Override with OCI_REF.
component_ref := env("OCI_REF", "actpkg.dev/library/servo")

act := env("ACT", "npx @actcore/act")
actbuild := env("ACT_BUILD", "npx @actcore/act-build")
hurl := env("HURL", "hurl")
# Random port for the e2e server, in a safe range: above the well-known/common
# dev ports and below the Linux outbound ephemeral range (32768+).
port := `shuf -i 10000-19999 -n 1`
addr := "[::1]:" + port
baseurl := "http://" + addr

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

build: init
    cargo build --target wasm32-wasip2 --release

# Embed act:component metadata and act:skill into the wasm.
pack: build
    {{actbuild}} pack {{wasm}}

test: pack
    #!/usr/bin/env bash
    set -euo pipefail
    # The engine keeps client storage under /tmp/servo and will not start
    # without it. The path is the engine's, not ours, so the grant names it.
    mkdir -p /tmp/servo
    {{act}} run {{wasm}} --http --listen "{{addr}}" \
      --grant '{"wasi:filesystem":{"mode":"allowlist","allow":[{"path":"/tmp/servo/**","mode":"rw"}]}}' &
    PID=$!
    trap 'kill $PID 2>/dev/null || true' EXIT
    curl --retry 120 --retry-connrefused --retry-delay 1 -fsS -o /dev/null {{baseurl}}/info
    {{hurl}} --test --variable "baseurl={{baseurl}}" e2e/*.hurl

publish: pack
    #!/usr/bin/env bash
    set -euo pipefail
    INFO=$({{act}} inspect component-manifest {{wasm}})
    VERSION=$(echo "$INFO" | jq -r .std.version)
    {{actbuild}} push {{wasm}} "{{component_ref}}:$VERSION" --skip-if-exists --also-tag latest
