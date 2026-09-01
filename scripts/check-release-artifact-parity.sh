#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/rust/video-interop/Cargo.toml" | head -n 1)"
hex_dir="$work_dir/hex"
cargo_target="$work_dir/cargo-target"
cargo_dir="$work_dir/cargo/video-interop-$version"

if [[ -z "$version" ]]; then
  echo "could not read the crate version" >&2
  exit 1
fi

(
  cd "$repo_root"
  mix hex.build --unpack --output "$hex_dir" >/dev/null
)

cargo_args=(package -p video-interop --target-dir "$cargo_target")
if [[ "${VIDEO_INTEROP_ALLOW_DIRTY:-0}" == "1" ]]; then
  cargo_args+=(--allow-dirty)
fi

(
  cd "$repo_root"
  cargo "${cargo_args[@]}" >/dev/null
)

mkdir -p "$work_dir/cargo"
tar -xzf "$cargo_target/package/video-interop-$version.crate" -C "$work_dir/cargo"

source_crate="$repo_root/rust/video-interop"
hex_crate="$hex_dir/rust/video-interop"

compare_path() {
  local expected="$1"
  local actual="$2"
  local label="$3"

  if ! diff -ru --strip-trailing-cr "$expected" "$actual"; then
    echo "$label differs from the repository source" >&2
    exit 1
  fi
}

for path in lib mix.exs README.md CHANGELOG.md LICENSE; do
  compare_path "$repo_root/$path" "$hex_dir/$path" "Hex package $path"
done

for path in README.md LICENSE src; do
  compare_path "$source_crate/$path" "$hex_crate/$path" "Hex crate $path"
  compare_path "$source_crate/$path" "$cargo_dir/$path" "Cargo crate $path"
done

compare_path "$source_crate/Cargo.toml" "$hex_crate/Cargo.toml" "Hex crate Cargo.toml"
compare_path "$source_crate/Cargo.toml" "$cargo_dir/Cargo.toml.orig" "Cargo crate Cargo.toml.orig"

for path in _build deps target priv/native; do
  if [[ -e "$hex_dir/$path" ]]; then
    echo "unexpected path in Hex package: $path" >&2
    exit 1
  fi
done

if find "$hex_dir" -type f \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' -o -name '*.beam' \) \
  -print -quit | grep -q .; then
  echo "Hex package contains a compiled artifact" >&2
  exit 1
fi

echo "Hex and Cargo production sources match the repository"
