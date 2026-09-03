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
5. Configure the protected GitHub `crates-io` and `hex` environments described
   below.
6. Set the release date in `CHANGELOG.md`.
7. Check that both manifests still use version 0.1.0.
8. Start from a clean checkout without sibling dependency overrides.

Do not publish either package from a developer workstation. The `publish-crate`
and `publish-hex` CI jobs are the only publication paths.

For the first publication, crates.io cannot yet attach a trusted publisher to
an existing crate. Create a short-lived crates.io API token restricted to the
`video-interop` crate name with permission to publish a new crate. Store it only
as the `CARGO_REGISTRY_TOKEN` environment secret in a GitHub environment named
`crates-io`. Configure required reviewer approval, prevent administrator bypass
if available, and restrict deployment to tags matching `v*`.

For Hex, sign in to <https://hex.pm>, verify the publishing account, and create
a short-lived key from <https://hex.pm/dashboard/keys> with API write
permission. Store it only as the `HEX_API_KEY` environment secret in a GitHub
environment named `hex`. Give that environment the same reviewer and `v*` tag
protections as `crates-io`.

Do not put registry tokens in this repository or in command arguments saved by
shell history.

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

## Publish the Rust crate through CI

Pushing `v0.1.0` starts the normal CI matrix. The `publish-crate` job depends on
all validation jobs and the exact-tag gate, then waits for approval on the
protected `crates-io` environment. Review the successful prerequisite jobs and
approve that environment deployment. CI runs:

```sh
cargo publish --locked --package video-interop
```

Do not run that command locally and do not upload a separately built archive.
If the publish job fails before crates.io accepts the crate, fix the issue in a
new commit and tag a new version; never move a public tag.

After crates.io accepts 0.1.0, create a temporary crate that depends on
`video-interop = "=0.1.0"`. Fetch it from crates.io and compile the default,
core-only, EGL, and Vulkan feature sets. Stop if the registry archive differs
from the tag.

The first release must use the short-lived token because trusted publishing can
only be configured after the crate exists. After 0.1.0 is published, configure
the crate's crates.io trusted publisher for organization `emerge-elixir`,
repository `video_interop`, workflow `ci.yml`, and environment `crates-io`.
Then migrate the CI job to `rust-lang/crates-io-auth-action`, remove the
long-lived secret, and revoke the bootstrap token.

## Publish Hex through CI

After crates.io accepts and the registry verification passes, approve the
pending `publish-hex` deployment on the protected `hex` environment. The job
cannot run until `publish-crate` succeeds. CI rechecks the package and docs,
then publishes both from the same tag with:

```sh
mix hex.publish package --yes
mix hex.publish docs --yes
```

Do not run those commands locally. After Hex accepts them, create a temporary
Mix project with:

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
