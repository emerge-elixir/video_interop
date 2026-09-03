# Releasing VideoInterop

VideoInterop publishes a Rust crate and a Hex package from the same commit.
Publish the crate first because the Hex package is used by projects whose native
code downloads it from crates.io.

## Before tagging

1. Make sure the GitHub repository is public.
2. Check that anonymous users can clone it and open the links in both package
   manifests.
3. Confirm that `main` contains every release fix and is not ahead of the remote.
4. Confirm that crates.io and Hex accounts are available for both package names.
5. Set the release date in `CHANGELOG.md`.
6. Check that both manifests still use version 0.1.0.
7. Start from a clean checkout without sibling dependency overrides.

Do not put registry tokens in this repository or in command arguments saved by
shell history. Configure Cargo credentials and run `mix hex.user auth` before
starting.

## Validate the source

```sh
mix deps.get
mix format --check-formatted
mix compile --force --warnings-as-errors
mix test
mix docs --warnings-as-errors

cargo fmt --all -- --check
cargo build --workspace --all-targets --all-features
cargo build --release --workspace --all-targets --all-features
cargo test --workspace
cargo test --workspace --all-features
cargo test -p video-interop --no-default-features
cargo test -p video-interop --no-default-features --features egl
cargo test -p video-interop --no-default-features --features vulkan
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p video-interop --no-default-features --all-targets -- -D warnings
cargo clippy -p video-interop --no-default-features --features egl --all-targets -- -D warnings
cargo clippy -p video-interop --no-default-features --features vulkan --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p video-interop --all-features --no-deps

scripts/check-vulkan-shaders.sh
scripts/check-release-artifact-parity.sh
```

Build and inspect both packages:

```sh
mix hex.build --unpack --output /tmp/video_interop-0.1.0
cargo package -p video-interop
cargo publish -p video-interop --dry-run
```

The Hex package must not contain `_build`, `deps`, `target`, `priv/native`,
compiled NIFs, BEAM files, or credentials. It must contain the Elixir source and
`rust/video-interop` production source. The Cargo package may also contain its
Rust integration tests.

Compile the unpacked Hex package:

```sh
(
  cd /tmp/video_interop-0.1.0
  MIX_ENV=prod mix compile --force --warnings-as-errors
  cargo test --manifest-path rust/video-interop/Cargo.toml --no-default-features
  cargo test --manifest-path rust/video-interop/Cargo.toml --all-features
)
```

Record the commit, toolchain versions, package file lists, and archive checksums
in `plans/release-0.1.0-audit.md`. `git status --short` must be empty.

## Tag

Create an annotated tag on the commit that produced both packages:

```sh
git tag -a v0.1.0 -m "Release VideoInterop 0.1.0"
git push origin main
git push origin v0.1.0
```

Wait for every CI job on `v0.1.0` to pass. Do not publish from a different
checkout or commit.

## Publish the Rust crate

```sh
cargo publish -p video-interop
```

After crates.io accepts it, create a temporary crate that depends on
`video-interop = "=0.1.0"`. Fetch it from crates.io and compile the default,
core-only, EGL, and Vulkan feature sets.

Stop if the registry archive differs from the tag.

## Publish Hex

```sh
mix hex.publish
```

After Hex accepts it, create a temporary Mix project with:

```elixir
{:video_interop, "== 0.1.0"}
```

Fetch it from Hex, compile with warnings denied, and check the generated HexDocs
source links.

Create the GitHub release from `v0.1.0`. Include the supported Vulkan scope and
platform requirements.

## Update downstream projects

Remove Cargo path patches and normal Mix path overrides from Emerge. Regenerate
both lock files from crates.io and Hex. Run Emerge's complete CI and compile its
unpacked Hex package without a sibling VideoInterop checkout.

Update other adapters only after that registry-only check passes.

## If publication fails

If the Cargo upload fails, fix the issue on a new commit and create a new version.
Do not move a public tag.

If the Rust crate publishes but Hex fails, leave the crate in place while fixing
the Hex package. If the crate itself is unsafe or unusable, yank it on crates.io
and document why. Yanking is not a substitute for deleting or moving the tag.

If Hex publishes a broken package, retire it in Hex and publish a new patch
version. Do not overwrite 0.1.0.
