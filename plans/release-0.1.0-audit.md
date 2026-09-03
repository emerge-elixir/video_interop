# VideoInterop 0.1.0 Release Audit

Status: **no-go from the current checkout; implementation and package checks pass, but release-state
and credential blockers remain**

Audit date: 2026-09-03

Candidate state: local release work based on `main`. The final pushed and dated release commit does
not exist yet.

## Decision

No source-code, ownership, synchronization, package-closure, or documentation defect found by this
audit blocks 0.1.0. Vulkan is part of the supported 0.1 contract. Remaining V3DV qualification is
platform follow-up and does not change the API status.

Publication cannot start from this checkout yet. The source must first be pushed, made public,
dated, tagged, and validated by exact-tag CI. Registry credentials are also unavailable in the
current environment.

## Release blockers

### 1. Final candidate is not pushed

Local release commits are not synchronized to `origin/main`. The candidate must include the Vulkan
support wording, CI-only crates.io publication job, release docs, Rust module documentation, and
this refreshed audit.

Before release:

1. commit the final local release changes and push them;
2. verify `git diff --check` and `git status --short` is empty;
3. run the complete release matrix from that exact commit.

Both published artifacts must be generated from this one clean commit.

### 2. The source repository is not publicly reachable

Anonymous access to <https://github.com/emerge-elixir/video_interop> currently returns HTTP 404.
The package metadata, README badges, source links, docs.rs metadata, HexDocs source links, and crate
README all point there.

Before tagging:

1. push all local commits to `origin/main`;
2. make the repository public;
3. verify anonymous clone, README badges, license, source links, and the Actions workflow;
4. confirm branch protection and tag workflow permissions.

### 3. Final date, tag, and public CI run do not exist

`CHANGELOG.md` still says `0.1.0 - Unreleased`, and there is no `v0.1.0` tag. This is correct until
the final release commit is ready.

Immediately before tagging:

1. replace `Unreleased` with the actual release date;
2. commit that release metadata;
3. create and push annotated tag `v0.1.0` on the same commit;
4. wait for every tag CI dependency and the `release-tag` job to pass.

The tag gate already verifies the tag name, Cargo version, Mix version, and dated changelog heading.

### 4. Registry and GitHub credentials are unavailable

The current environment has no `HEX_API_KEY`, Cargo registry token, GitHub token, Cargo credentials
file, or authenticated Hex configuration. `mix hex.publish --dry-run` reaches authentication and
then fails because no Hex user is configured.

The first crates.io publication must run through the protected `publish-crate` CI job. Because the
crate does not exist yet, bootstrap that job with a short-lived, crate-name-restricted token carrying
new-crate publication permission in the `crates-io` GitHub environment. Configure trusted
publishing and revoke the bootstrap token immediately after 0.1.0 exists.

Authenticate outside the repository before the release session. Do not put tokens in files tracked
by Git or in command arguments retained by shell history.

## Registry state

Checked on 2026-09-03:

- crates.io reports that `video-interop` does not exist;
- Hex reports that `video_interop` does not exist.

The names are currently available but are not reserved. Recheck immediately before publishing.
Publish `video-interop` first, then `video_interop`.

## Validation performed

### Elixir and OTP

Passed on both supported combinations:

- Elixir 1.17.3 / OTP 27.3.4.3;
- Elixir 1.20.2 / OTP 29.0.5.

Checks:

```sh
mix deps.get
mix format --check-formatted
mix compile --force --warnings-as-errors
mix test
mix docs --warnings-as-errors
mix hex.audit
```

Result: 113 ExUnit tests pass. The invalid-reservation lifecycle test intentionally logs one
supervised `LeaseOwner` termination while asserting the failure boundary.

### Rust

Passed on Rust 1.91.0 and current stable 1.98.0:

```sh
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
```

The all-feature crate run passes 47 unit tests, 8 descriptor integration tests, 2 FD ownership
tests, schema fixture builds, and doctests.

### Shaders

The pinned glslang 16.4.0 check passes for all three compute shaders. Regenerated SPIR-V is
byte-identical to the committed artifacts and passes `spirv-val` for Vulkan 1.1.

### Hex package

`mix hex.build --unpack` produces 54 files containing only the intended Elixir source, package
documentation, license, and canonical embedded Rust crate source. It contains no `_build`, `deps`,
workspace `target`, native library, BEAM file, or sibling path dependency.

The unpacked package passes warnings-as-errors production compilation. Its embedded crate passes
core-only and all-feature tests using registry dependencies.

The provisional archive built from the current working tree is 112,640 bytes. Its checksum is not
a release checksum because the changelog date and final commit still need to change.

### Cargo package

`cargo publish -p video-interop --dry-run --allow-dirty` passes packaging, registry dependency
resolution, and crate verification. The package contains 31 files, including the canonical source,
integration tests, GLSL, and SPIR-V artifacts. It has no sibling path dependency.

The `video-interop` and `video_interop` package production sources match. The parity check passes
both in the current working tree with the explicit audit override and in a clean temporary Git
snapshot without an override.

### Metadata and dependencies

- Mix and Cargo versions both equal `0.1.0`.
- The crate declares Rust 1.91 and edition 2024.
- The Hex package declares Elixir `~> 1.17`.
- The Hex package has no production dependency.
- The published crate uses registry dependencies only.
- Root and crate Apache-2.0 license files are byte-identical.
- `mix hex.audit` reports no retired or vulnerable Hex dependencies.
- ExDoc 0.40.4 is available while the development lock uses 0.40.3; this is optional and not a
  release blocker.

### Downstream source integration

The current local VideoInterop source passes with:

- Emerge: 1,007 Rust tests and 435 Elixir tests;
- `membrane_video_interop`: 12 tests;
- `membrane_video_transcode`: 10 tests;
- `emerge_demo`: 58 tests.

Registry-only downstream validation remains necessarily blocked until both packages are published.

## Non-blocking follow-up

- Continue pinned-RPi5/V3DV pixel, validation, fault-injection, restart, and soak qualification.
- Continue behavior-neutral Vulkan module decomposition if desired; do not delay 0.1.0 solely for
  that refactor.
- Add automated dependency advisory tooling later if desired; current CI already validates package
  closure, minimum toolchains, features, shaders, and source parity.

## Exact remaining sequence

1. Push `main`, make the GitHub repository public, and verify anonymous access.
2. Create the protected GitHub `crates-io` environment and its bootstrap token secret.
3. Run `RELEASING.md` from a clean public clone.
4. Set the final changelog date and commit it.
5. Recheck both registry names.
6. Create and push annotated `v0.1.0`; wait for all validation and exact-tag CI jobs.
7. Approve the protected `publish-crate` job so CI publishes `video-interop`.
8. Fetch `video-interop = "=0.1.0"` from crates.io and test core, default, EGL, and Vulkan features.
9. Configure crates.io trusted publishing, remove the CI secret, and revoke the bootstrap token.
10. Run `mix hex.publish` from the same tagged checkout.
11. Fetch `{:video_interop, "== 0.1.0"}` in a clean Mix project and verify compilation and HexDocs.
12. Create the GitHub release.
13. Remove downstream path overrides, regenerate locks, and run registry-only Emerge and adapter CI.

## Release decision

**No-go from the current checkout.** The implementation and package artifacts are release-ready.
The remaining blockers are final source control state, public repository access, exact-tag CI, and
registry authentication/publication.
