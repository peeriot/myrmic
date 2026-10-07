#!/usr/bin/env bash
#
# nextest setup script (see `.config/nextest.toml`): builds every cell fixture
# in `tests/fixtures` once, before any test that links `sorg-tests` starts.
# Tests used to compile their fixture themselves, but all those builds share
# the target dir's cargo lock, so under a full run each test spent most of its
# time budget waiting for the others' builds.
#
# Hands the tests a `PATH`-style list of `<fixture dir>=<wasm module>` entries
# via `$NEXTEST_ENV`; `sorg_tests::register_fixture_class` looks its fixture up
# there. nextest runs this from the workspace root.

set -euo pipefail

target_dir=$(cargo metadata --no-deps --format-version 1 |
    grep -o '"target_directory":"[^"]*"' | cut -d'"' -f4)

cargo build --quiet -p myrmic-cli --bin myrmic

# Every cell depends on `myrmic-sdk`; the fixtures that don't are plain
# support libraries for the cells (e.g. `module-examples-common`). Each cell is
# keyed by its `heap_size` (empty when it keeps the default): the heap size is
# compiled into `myrmic-sdk`, so building cells grouped by it rebuilds the SDK
# once per distinct size instead of on every change between neighbours.
cells=()
for dir in tests/fixtures/*/; do
    dir=$(realpath "$dir")
    if grep -Eq '^myrmic-sdk\s*=' "$dir/Cargo.toml"; then
        heap_size=$(sed -n 's/^heap_size\s*=\s*//p' "$dir/Cargo.toml")
        cells+=("$heap_size"$'\t'"$dir")
    fi
done
mapfile -t cells < <(printf '%s\n' "${cells[@]}" | LC_ALL=C sort -t $'\t' -k1,1)

entries=()
for cell in "${cells[@]}"; do
    dir=${cell#*$'\t'}
    "$target_dir/debug/myrmic" build "$dir"

    # `cargo pkgid` omits the package name when it equals the directory name.
    fragment=$(cargo pkgid --manifest-path "$dir/Cargo.toml")
    fragment=${fragment##*#}
    if [[ $fragment == *@* ]]; then
        package=${fragment%@*}
    else
        package=$(basename "$dir")
    fi

    wasm="$target_dir/wasm32v1-none/release/${package//-/_}.wasm"
    if [[ ! -f $wasm ]]; then
        echo "built $dir, but found no wasm module at $wasm" >&2
        exit 1
    fi
    entries+=("$dir=$wasm")
done

(IFS=:; echo "SORG_TESTS_CELL_FIXTURES=${entries[*]}") >> "$NEXTEST_ENV"
