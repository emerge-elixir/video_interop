# VideoInterop 0.1.0 Release Preparation Plan

Status: local implementation and release-candidate checks complete; public repository, tag,
publication, and downstream registry work remain

Related audit: [`release-0.1.0-audit.md`](release-0.1.0-audit.md)

Completed locally:

- compiler warning fix and Elixir 1.17/1.20 checks;
- Rust 1.91/latest debug, release, test, Clippy, Rustdoc, feature, and package checks;
- unpacked Hex compilation and embedded-crate tests;
- package source parity and shader checks;
- public documentation rewrite and Vulkan experimental policy;
- maintainer release checklist.

Waiting on maintainer or external state:

- push the candidate;
- make GitHub public and verify links;
- run exact-tag CI and publish both packages;
- migrate Emerge to registry-only dependencies;
- complete V3DV hardware qualification before removing the experimental label.

## Goal

Publish one clean, public, reproducible `v0.1.0` commit as both:

- crates.io crate `video-interop` 0.1.0;
- Hex package `video_interop` 0.1.0.

Then move Emerge and downstream adapters from sibling path overrides to those exact registry
artifacts.

## Release policy

Recommended 0.1 scope:

- accept the Elixir descriptor, validation, lease, consumer, and ownership contracts;
- accept the Rust core, Rustler schema, fd ownership, and EGL adapter contracts;
- publish the Vulkan adapter as **experimental** until the pinned-RPi5 qualification matrix is
  complete;
- retain SemVer 0.x freedom while documenting every ownership boundary as correctness-critical.

If Vulkan is to be advertised as stable instead, stop before publication and complete the hardware
qualification gate in Phase 6.

## Release gates

- Both artifacts come from one clean, public, tagged commit.
- Source and unpacked packages compile with warnings denied.
- Rust source embedded in Hex matches the crates.io source archive.
- The advertised Elixir and Rust minimum versions are tested.
- No package depends on sibling repositories, unpublished packages, or path patches.
- Publication is crate first, Hex second, downstream lock regeneration last.
- Do not publish automatically merely because an artifact build succeeds.

## Phase 1 — Eliminate compiler warnings

Files:

- `lib/video_interop.ex`
- relevant consumer/session tests
- `.github/workflows/ci.yml`

Work:

1. Replace direct closed-world `Consumer.impl_for/1` and `ConsumerSession.impl_for/1` conditionals
   with one warning-free dynamic protocol-availability helper. A likely implementation uses
   `apply(protocol, :impl_for, [value])` and checks explicitly for `nil`, preserving support for
   implementations compiled in downstream applications.
2. Preserve current behavior:
   - unsupported consumer returns `{:error, {:unsupported_consumer, value}}`;
   - unsupported transfer releases the caller-owned frame exactly once;
   - an opened value without `ConsumerSession` raises `ConsumerContractError`;
   - unsupported close raises `ConsumerContractError`.
3. Add or retain positive tests using external test-only protocol implementations and negative tests
   for unsupported values.
4. Add this mandatory CI step before tests:

   ```bash
   mix compile --force --warnings-as-errors
   ```

Exit gate:

```bash
mix clean
mix compile --force --warnings-as-errors
mix test
```

passes without suppressing type warnings.

## Phase 2 — Define and test compatibility floors

Files:

- `mix.exs`
- `.tool-versions`
- `rust/video-interop/Cargo.toml`
- `.github/workflows/ci.yml`
- both READMEs

Elixir:

1. Add a minimum job using Elixir 1.17 and a compatible OTP release, initially OTP 27.
2. Keep the current Elixir 1.20/OTP 29 job.
3. Run compile-with-warnings-denied and tests in both jobs.
4. Prefer fixing small compatibility issues. If 1.17 support requires substantial compatibility
   code, narrow `mix.exs` to the version actually maintained and document that decision.

Rust:

1. Keep `rust-version = "1.91"` and the exact Rust 1.91 CI job.
2. Add a latest-stable job for forward compatibility.
3. Run core, default/Rustler, EGL, Vulkan, and all-feature checks at the appropriate matrix points.
4. Keep formatting and warnings-denied Clippy on the pinned release compiler.

Exit gate: every version claimed in package metadata has a passing CI job.

## Phase 3 — Strengthen source, package, documentation, and shader CI

Files:

- `.github/workflows/ci.yml`
- new release/parity scripts under `scripts/`
- `mix.exs`
- Rust and Elixir documentation

### Elixir/Hex gates

1. Run:

   ```bash
   mix format --check-formatted
   mix compile --force --warnings-as-errors
   mix test
   mix docs --warnings-as-errors
   ```

2. Build and unpack Hex into a temporary directory.
3. In the unpacked package, run a clean production compile with warnings denied.
4. Compile and test the embedded `rust/video-interop` crate with core and all features.
5. Verify the package contains no `_build`, `deps`, host fixture NIFs, Cargo target output, secrets, or
   release credentials.

### Cargo gates

CI must compile, lint, and test the complete Rust workspace. Keep these as separate commands so a
passing test build cannot hide a missing normal build or a skipped Clippy target:

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets --all-features
cargo build --release --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Also run the crate without default features and with each optional graphics feature:

```bash
cargo test -p video-interop --no-default-features
cargo test -p video-interop --no-default-features --features egl
cargo test -p video-interop --no-default-features --features vulkan
cargo clippy -p video-interop --no-default-features --all-targets -- -D warnings
cargo clippy -p video-interop --no-default-features --features egl --all-targets -- -D warnings
cargo clippy -p video-interop --no-default-features --features vulkan --all-targets -- -D warnings
```

Then:

1. Remove `--allow-dirty` from `cargo package` in clean CI.
2. Test the generated crate archive with core and Vulkan features.
3. Run Rustdoc with warnings denied:

   ```bash
   RUSTDOCFLAGS="-D warnings" cargo doc -p video-interop --all-features --no-deps
   ```

### Cross-artifact parity

Add `scripts/check-release-artifact-parity.sh` that:

1. builds/unpacks the Hex archive;
2. builds/extracts the Cargo archive;
3. compares production `src/`, GLSL, SPIR-V, README, and LICENSE content against the repository;
4. compares the Cargo archive's `Cargo.toml.orig` with the manifest embedded in Hex;
5. allows crates.io-only integration tests and Cargo-generated normalized metadata;
6. fails on any production-source divergence.

### Documentation metadata

1. Add explicit `source_url` and tag-based `source_ref` for ExDoc.
2. Verify all package links anonymously after the repository becomes public.
3. Keep reproducible shader byte comparison and `spirv-val` validation mandatory.

Exit gate: source, unpacked Hex, generated crate, docs, and shader jobs all pass from a clean clone.

## Phase 4 — Finalize the public 0.1 contract and documentation

### Writing style

Use `../solve/README.md` as the style reference:

- start with a short statement of what the library does;
- put installation and a small working example before architecture details;
- explain one idea at a time in plain language;
- use concrete outcomes such as “returns an error and keeps ownership with the caller” instead of
  abstract policy labels;
- use short paragraphs, examples, and tables where they help;
- keep ownership and synchronization terms only where they are technically necessary;
- avoid inflated wording and repeated adjectives such as “canonical”, “truthful”, “immutable”,
  “strict”, and “hardened” when a direct description is clearer;
- do not turn implementation history into user documentation.

Rewrite existing uses of “canonical allocation size” as “the allocation size reported by the fd”,
“truthful allocation” as “the complete allocation”, and similar phrases throughout the public
README, changelog, and module documentation.

Files:

- `README.md`
- `rust/video-interop/README.md`
- `CHANGELOG.md`
- public Elixir modules
- public Rust modules and items
- Emerge's public setup/architecture documentation

Work:

1. Lead the changelog with user-facing scope rather than implementation chronology:
   - supported frame/storage schemas;
   - lease and ownership behavior;
   - Rustler boundary support;
   - EGL support;
   - Vulkan experimental status or completed qualification;
   - Linux/process-local fd limitations;
   - Elixir/Rust minimum versions.
2. Add the final release date only when tagging.
3. Document every public operation that transfers, retains, claims, retires, or abandons ownership.
4. Document raw EGL/Vulkan handle lifetime, queue authority, external synchronization, and
   destruction preconditions.
5. Add item-level Rust documentation where ownership, raw handles, synchronization, or cleanup can
   be misunderstood. Keep the remaining experimental Vulkan fields out of a crate-wide
   missing-docs gate until that API settles after 0.1.
6. Ensure ExDoc describes nonstandard `LeaseOwner.start_link/1` producer linking,
   `start_supervised/1`, reservation ownership boundaries, callback execution, retries, and drain.
7. State that fd integers are local borrowed handles and are not serializable or Erlang-node-safe.
8. Update Emerge to use exactly the same Vulkan stability wording.

Exit gate: a new producer and consumer can implement the contract from public docs without reading
internal plans.

## Phase 5 — Publish the repository and establish release controls

Repository state currently requiring action:

- local `main`: `09a14a3`;
- recorded `origin/main`: `194b34b`;
- public anonymous GitHub/API access: 404;
- release tags: none.

Work:

1. Commit audit/remediation changes in reviewable commits.
2. Push all commits, including `09a14a3`.
3. Make `emerge-elixir/video_interop` public.
4. Verify anonymously:
   - clone/fetch;
   - default branch and commit history;
   - README images and links;
   - license;
   - issue tracker and security contact policy;
   - package repository/documentation links.
5. Protect `main` and require the release CI jobs.
6. Add `RELEASING.md` with exact commands, expected package contents, credential prerequisites,
   rollback windows, and registry verification commands.
7. Keep publication manual for 0.1.0 unless a dedicated workflow requires successful exact-tag CI
   and environment approval. Never let a generic artifact job publish directly.
8. Configure crates.io and Hex credentials outside the repository and verify package ownership
   accounts before tagging.

Exit gate: anonymous users can inspect the exact candidate source and maintainers can authenticate
to both registries without storing credentials in Git or shell history.

## Phase 6 — Resolve Vulkan qualification policy

### Recommended fast release path

Mark Vulkan experimental for 0.1.0 and preserve the existing host safety tests. Record that pinned
hardware qualification remains required before declaring it stable.

### Stable Vulkan alternative

Complete the existing pinned-RPi5 matrix before continuing:

- exact-pixel NV12 and packed fixtures;
- Vulkan validation and V3DV MMU/kernel logs;
- acquire/release failure injection;
- replacement, hotplug, stream restart, renderer restart, and cold boot;
- 10,000-frame and long-duration FD/RSS/cache/pool/queue soaks;
- confirmed `LinearBufferToOptimalYuvPlanes` production strategy;
- throughput target with at least 30% GPU headroom.

Exit gate: documentation accurately reflects the chosen policy and no unqualified stability claim
remains.

## Phase 7 — Clean release-candidate validation

Create a fresh clone that has no sibling VideoInterop/Emerge development directories on its search
paths.

Run:

```bash
mix deps.get
mix format --check-formatted
mix compile --force --warnings-as-errors
mix test
mix docs --warnings-as-errors
mix hex.build --unpack

cargo fmt --all -- --check
cargo build --workspace --all-targets --all-features
cargo build --release --workspace --all-targets --all-features
cargo test --workspace
cargo test --workspace --all-features
cargo test -p video-interop --no-default-features
cargo test -p video-interop --no-default-features --features egl
cargo test -p video-interop --no-default-features --features vulkan
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p video-interop --all-features --no-deps
scripts/check-vulkan-shaders.sh
scripts/check-release-artifact-parity.sh
```

Also:

1. compile the unpacked Hex package;
2. compile/test its embedded Rust crate;
3. run `cargo publish -p video-interop --dry-run` without `--allow-dirty`;
4. run the Hex local build checks without publishing;
5. confirm `git status --short` is empty afterward.

Exit gate: record exact commands, toolchain versions, package file lists, archive checksums, and
source commit in the release audit.

## Phase 8 — Tag and publish 0.1.0

1. Recheck that both registry names are still available.
2. Set the changelog date and commit only release metadata.
3. Confirm versions match exactly in:
   - `mix.exs`;
   - `rust/video-interop/Cargo.toml`;
   - changelog heading;
   - package validation paths/scripts.
4. Create signed or annotated tag `v0.1.0` at the clean candidate commit.
5. Push the commit and tag, then wait for exact-tag CI to pass.
6. Publish the Rust crate first:

   ```bash
   cargo publish -p video-interop
   ```

7. In a clean temporary crate, fetch `video-interop = "=0.1.0"` from crates.io and test core,
   Rustler, EGL, and Vulkan feature combinations.
8. Publish Hex:

   ```bash
   mix hex.publish
   ```

9. In a clean temporary Mix project, fetch `{:video_interop, "== 0.1.0"}`, compile with warnings
   denied, and verify HexDocs.
10. Create the GitHub release from the same tag with user-facing notes and experimental limitations.

Stop if either artifact differs from the tagged source. Do not publish downstream packages against
an unverified or yanked dependency.

## Phase 9 — Convert Emerge and downstream projects to registry-only dependencies

Emerge integration branch:

1. remove the unconditional Cargo `[patch.crates-io]` sibling path;
2. retain a development override only if it is explicit and absent from normal package resolution;
3. fetch the Hex package without `VIDEO_INTEROP_PATH`;
4. regenerate `mix.lock` and `native/emerge_skia/Cargo.lock` from registries;
5. verify crate source/checksum entries in Cargo.lock and Hex source/checksum in mix.lock;
6. run `./ci-tests.sh all`;
7. build/unpack Emerge's Hex archive and compile it without any sibling checkout.

Downstream adapters:

1. update to exact compatible 0.1 constraints;
2. regenerate locks from public registries;
3. run clean-checkout host suites;
4. publish only after Emerge and adapter registry-only tests pass.

Exit gate: Emerge 0.4 and every initially published adapter build from public registries alone.

## Commit sequence

Suggested commits:

1. `Fix dynamic consumer protocol checks`
2. `Enforce release compiler and package gates`
3. `Document the VideoInterop 0.1 contract`
4. `Prepare VideoInterop 0.1.0 release`

Keep the final release metadata commit limited to version/date/checklist adjustments. Do not mix API
or ownership changes into the tag commit.

## Completion criteria

VideoInterop 0.1.0 is ready only when:

- all release blockers in `release-0.1.0-audit.md` are resolved;
- source and both package archives compile without warnings;
- minimum and current toolchains pass;
- artifact parity and shader reproducibility pass;
- GitHub is public and contains the exact candidate commit;
- Vulkan is qualified or explicitly experimental;
- exact-tag CI passes;
- crates.io and Hex artifacts are independently fetched and verified;
- Emerge passes registry-only full CI and package-source compilation;
- the release audit records final commit, tag, archive checksums, and validation results.
