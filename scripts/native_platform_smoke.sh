#!/usr/bin/env bash
# Every way a program runs as native code, on this host: JIT, archive, the JIT
# runner and the AOT executable. Each must print the same, and an AOT package
# must be one: `coil package` succeeds when it falls back to the JIT runner.
#
#   scripts/native_platform_smoke.sh TARGET_DIR STDLIB_SRC
set -euo pipefail

bin="$1"
stdlib="$2"
ext=""
[[ -x "$bin/coil.exe" ]] && ext=".exe"
coil="$bin/coil$ext"
work="$(mktemp -d)"
# Best-effort cleanup: Windows can keep a just-exited coil.exe locked for a
# moment, and under `set -e` a failed trap would turn a passing run into exit 1
# (#2313).
cleanup() {
    rm -rf "$work" 2>/dev/null || { sleep 2; rm -rf "$work" 2>/dev/null; } || true
}
trap cleanup EXIT

cat > "$work/app.coil" <<'COIL'
use io::sync::println;
use string::format;
use task::{scope, Scope, Channel};

fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

gen fn squares(int n) {
    let i = 0;
    while i < n {
        yield i * i;
        i = i + 1;
    }
}

fn main() {
    let g = squares(5);
    let t = 0;
    while !done(g) {
        t = t + resume g;
    }
    let ch: Channel<int> = task::channel(1);
    let r = scope(fn (Scope s) use (ch) {
        s.spawn(fn () use (ch) {
            let _ = ch.send(fib(20));
            ch.close();
            0
        });
        return match ch.recv() {
            Result::Ok(v) => v,
            Result::Err(_) => -1,
        };
    });
    let _ = println(format("%i %i %i", fib(25), t, match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    }));
}
COIL
want="$("$coil" --no-build-cache --mir --root "$stdlib" "$work/app.coil")"
echo "MIR interpreter: $want"

check() {
    local what="$1" got
    shift
    got="$("$@")"
    if [[ "$got" != "$want" ]]; then
        echo "::error::$what printed '$got', not '$want'"
        exit 1
    fi
    echo "ok: $what"
}

check "JIT run" "$coil" --no-build-cache --root "$stdlib" "$work/app.coil"
"$coil" compile --root "$stdlib" "$work/app.coil" -o "$work/app.ccoil"
check "archive run" "$coil" run "$work/app.ccoil"

"$coil" package --no-aot --root "$stdlib" "$work/app.coil" -o "$work/runner$ext"
check "JIT runner" "$work/runner$ext"

aot() {
    local out="$1" said
    shift
    said="$(env "$@" "$coil" package --root "$stdlib" "$work/app.coil" -o "$out" 2>&1)"
    echo "$said"
    if [[ "$said" == *"packaging the JIT runner"* ]]; then
        echo "::error::coil package fell back to the JIT runner"
        exit 1
    fi
}

aot "$work/aot$ext" "COIL_RT_LIB=$bin/libcoil_rt.a"
check "AOT executable" "$work/aot$ext"

# What ships must run where no toolchain is installed: every test above has
# MSYS2's or Homebrew's libraries in reach.
standalone() {
    local file="$1" deps
    if [[ -n "$ext" ]]; then
        deps="$(objdump -p "$file" | sed -n 's/^[[:space:]]*DLL Name: //p' | tr '[:upper:]' '[:lower:]' | sort -u)"
        echo "$file needs: $(echo "$deps" | tr '\n' ' ')"
        deps="$(echo "$deps" | grep '^lib' || true)"
    elif [[ "$(uname)" == Darwin ]]; then
        deps="$(otool -L "$file" | sed 1d | awk '{print $1}')"
        echo "$file needs: $(echo "$deps" | tr '\n' ' ')"
        deps="$(echo "$deps" | grep -vE '^(/usr/lib/|/System/)' || true)"
    else
        return 0
    fi
    if [[ -n "$deps" ]]; then
        echo "::error::$file needs libraries a user may not have: $deps"
        exit 1
    fi
}
standalone "$work/aot$ext"
standalone "$work/runner$ext"
standalone "$coil"
if [[ -n "$ext" ]]; then
    check "AOT executable with only Windows on PATH" env PATH="$(cygpath -u "$WINDIR")/System32" "$work/aot$ext"
fi

# As a release does it: the runtime inside `coil`, with none beside it.
mkdir "$work/release"
cp "$coil" "$work/release/coil$ext"
"$(command -v python3 || command -v python)" scripts/embed_runtime.py "$work/release/coil$ext" "$bin/libcoil_rt.a"
coil="$work/release/coil$ext"
aot "$work/embedded$ext" "XDG_CACHE_HOME=$work/cache"
check "AOT executable from the embedded runtime" "$work/embedded$ext"
