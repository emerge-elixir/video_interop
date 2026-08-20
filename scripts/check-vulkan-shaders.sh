#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
shader_dir="$root/rust/video-interop/src/vulkan"
target_env="vulkan1.1"
expected_glslang="16.4.0"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

command -v glslangValidator >/dev/null || {
  echo "glslangValidator is required" >&2
  exit 1
}
command -v spirv-val >/dev/null || {
  echo "spirv-val is required" >&2
  exit 1
}

if [[ "${VIDEO_INTEROP_REQUIRE_PINNED_SHADER_TOOLS:-0}" == "1" ]] &&
   ! glslangValidator --version | grep -Fq "$expected_glslang"; then
  echo "expected glslang $expected_glslang" >&2
  glslangValidator --version >&2
  exit 1
fi

for name in nv12 nv12_planes packed_to_bgra; do
  source="$shader_dir/$name.comp"
  committed="$source.spv"
  generated="$tmp/$name.comp.spv"
  glslangValidator -V --target-env "$target_env" -S comp -o "$generated" "$source" >/dev/null
  spirv-val --target-env "$target_env" "$generated"
  if ! cmp --silent "$committed" "$generated"; then
    echo "$committed is stale; run scripts/regenerate-vulkan-shaders.sh with glslang $expected_glslang" >&2
    exit 1
  fi
done
