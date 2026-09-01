# VideoInterop 0.1.0 Release Audit

Status: release preparation passes locally; publication is still blocked by repository and release
operations.

Audit date: 2026-09-01

Candidate implementation: `2d82d1958c6cd2693982901794dc9af22e87d7d7`

This candidate includes `09a14a3` and the local compiler, CI, package, and documentation fixes.

## Scope

This audit covers the first public release of:

- Hex package `video_interop` 0.1.0;
- crates.io crate `video-interop` 0.1.0.

It checks compiler warnings, supported toolchains, tests, documentation, package contents, source
parity, CI, repository access, hardware claims, and downstream release order.

## Summary

The source and both package formats now pass the local release checks. The Elixir compiler warnings
were removed, Elixir 1.17/OTP 27 was tested, Rust 1.91 and latest stable passed the complete build,
test, Clippy, and Rustdoc matrix, and the unpacked Hex package compiles without sibling files.
Generated Hex and Cargo packages contain the same production Rust source.

The public docs now introduce the library through short examples and describe Vulkan as
experimental. This allows the core, Rustler, lease, consumer, and EGL contracts to ship without
claiming that the remaining V3DV hardware work is complete.

Do not publish yet. The GitHub repository still returns 404 to anonymous users, local `main` is
ahead of `origin/main`, and no release tag exists. Registry credentials and exact-tag CI also remain
external release steps.

## Findings

### 1. Elixir warnings-as-errors failure — resolved locally

Elixir 1.20 previously reported four type warnings around direct `Consumer.impl_for/1` and
`ConsumerSession.impl_for/1` calls. Protocol lookup now goes through a dynamic helper, preserving
support for implementations compiled by downstream applications.

These commands now pass for the source and unpacked Hex package:

```sh
mix compile --force --warnings-as-errors
mix test
```

CI now runs warnings-as-errors compilation before tests.

### 2. Source repository and release commit are not public — open

Package metadata points to:

```text
https://github.com/emerge-elixir/video_interop
```

Anonymous GitHub requests return 404. The recorded branch state is:

```text
2d82d19 local main
194b34b origin/main
```

Before publishing:

1. push all commits, including `09a14a3`;
2. make the repository public;
3. check anonymous clone and every package/documentation link;
4. tag only the public commit used to build both packages.

### 3. Vulkan release policy — resolved for 0.1

The root README, crate README, changelog, Rust Vulkan module, and Emerge integration guide now call
Vulkan experimental. Pinned-RPi5 pixel, validation/MMU, synchronization-fault, restart, and soak
testing remains required before changing that label.

### 4. Toolchain floors — resolved locally and in CI

The package still declares Elixir `~> 1.17`. The complete 104-test suite passes on:

- Elixir 1.17.3 / OTP 27;
- Elixir 1.20.2 / OTP 29.

The crate declares Rust 1.91. The debug build, release build, test matrix, Clippy matrix, and Rustdoc
pass on:

- Rust 1.91.0;
- latest stable used by this audit, Rust 1.98.0.

CI now contains both Elixir combinations and both Rust toolchains.

### 5. Publication is manual and exact-tag verification is pending — open

`RELEASING.md` now records the validation, tag, publication, registry verification, downstream,
yank, and failure steps. Publication remains manual and requires successful CI on `v0.1.0`. A
tag-only CI job checks that the tag, both package versions, and dated changelog heading agree.

The current environment has no authenticated Hex user. A prior crates.io dry run passed. Recheck
both registry names immediately before tagging; availability is not a reservation.

### 6. CI release gates — resolved locally

`.github/workflows/ci.yml` now requires:

- Elixir formatting, warnings-as-errors compilation, and tests on minimum and current versions;
- ExDoc with warnings denied;
- unpacked Hex compilation;
- core and all-feature builds/tests for the Rust source embedded in Hex;
- debug and release Rust workspace builds with every target and feature;
- workspace, core-only, EGL-only, Vulkan-only, and all-feature tests;
- workspace/all-feature and per-feature Clippy with `-D warnings`;
- Rustdoc with `-D warnings`;
- a clean `cargo package` and tests from the generated crate;
- Rust 1.91 and latest stable;
- reproducible and validated SPIR-V;
- tag, package-version, and changelog-date agreement before a tag run can pass.

The new package parity script rejects dirty production-source differences and compiled files in the
Hex archive. The workflow itself still needs a public GitHub run before tagging.

### 7. Public documentation — resolved for the initial release

The READMEs now follow the direct, example-led style used by Solve. Installation and a small frame
example come before implementation details. Dense implementation history was removed from the
changelog.

The docs now cover:

- process-local fd limits;
- caller and consumer ownership after transfer;
- lease fan-out and draining;
- native prepare/claim behavior;
- dispatcher shutdown;
- EGL display, context, function, and thread responsibility;
- Vulkan experimental status and caller responsibility;
- supported Elixir and Rust versions.

ExDoc now has a public source URL and version-tag source ref. Rustdoc passes with warnings denied on
Rust 1.91 and latest stable. More item-level Vulkan documentation can be added as that experimental
API settles; it is not a 0.1 publication blocker.

### 8. Cross-package source parity — resolved locally

`scripts/check-release-artifact-parity.sh` builds and unpacks both package formats. It compares:

- Elixir source, root manifests, README, changelog, and license in the Hex package;
- crate manifest, source, GLSL, SPIR-V, README, and license in Hex;
- the same Rust production files and `Cargo.toml.orig` in the Cargo archive.

It also rejects `_build`, `deps`, `target`, `priv/native`, NIF libraries, and BEAM files in Hex.
The check passes from a clean temporary Git repository without `--allow-dirty`.

## Validation results

### Elixir

Passed:

```sh
mix format --check-formatted
mix compile --force --warnings-as-errors
mix test
mix docs --warnings-as-errors
```

Result: 104 tests on both supported Elixir/OTP combinations. The lifecycle test intentionally logs
one supervised process crash while verifying an invalid issue reservation.

### Rust

Passed on Rust 1.91 and Rust 1.98:

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

All-feature result: 42 crate unit tests, 8 descriptor tests, 2 fd tests, schema fixture builds, and
doctests.

A descriptor cleanup test was made independent of fd-number reuse after the expanded feature matrix
exposed its race.

### Packages and shaders

Passed:

- unpacked Hex production compilation with warnings denied;
- embedded crate core and all-feature tests;
- clean Cargo package verification;
- generated crate core and Vulkan tests;
- Hex/Cargo production-source parity;
- reproducible shader byte comparison and `spirv-val` validation.

Current Cargo archive: 31 files, about 390.5 KiB unpacked and 75.2 KiB compressed.

### Downstream

The reconciled Emerge 0.4 integration tree previously passed `./ci-tests.sh all` against this local
source: 462 Elixir tests, 1,005 Rust tests, Clippy, Credo, and Dialyzer.

Registry-only downstream validation cannot run until both packages are published.

## Remaining release steps

1. Push `main`, make the repository public, and verify anonymous access.
2. Add the release date to `CHANGELOG.md`.
3. Run the release checks from a clean public clone.
4. Create and push `v0.1.0`; wait for every tag CI job.
5. Recheck package-name availability and publish `video-interop` first.
6. Fetch and test all crate features from crates.io.
7. Publish `video_interop`, then compile a clean project from Hex.
8. Remove Emerge's normal path patches, regenerate both lock files, and run registry-only Emerge
    CI and package compilation.

## Release decision

**No-go for publication from the current local branch.** Local source, documentation, CI, and
package blockers are resolved. Public repository access, exact-tag CI, credentials, registry
publication, and downstream registry-only validation still require maintainer action.
