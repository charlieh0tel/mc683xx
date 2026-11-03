# Releasing

How a version reaches crates.io and PyPI.  One version covers the
workspace: every published crate and the `mc683xx` Python package.
The maintainer pushes the tags; a `v*` tag publishes, with no
approval step, once the `release` workflow's gate passes (Cutting a
release, step 7).

## What is published

| Package | Where | Built by |
| --- | --- | --- |
| `mc683xx-target`, `mc683xx-cpu32`, `mc683xx-modules`, `mc683xx-mc68331`, `mc683xx-board`, `mc683xx-gdb`, `mc683xx-debug`, `mc683xx-peripherals`, `mc683xx-run` | crates.io | `cargo publish --workspace`: the first release by hand, then the `release` workflow |
| `mc683xx` (the Python package, from `crates/cpu32-py`) | PyPI | `.github/workflows/release.yml`, from a `v*` tag |

Every other member has `publish = false`: `cpu32-py` (its wheel is its
release), `cpu32-harness`, `cpu32-oracle`, `snapshot-golden`,
`mc683xx-bench` and `mc683xx-examples`.

The published packages carry no tests that build.  A published
crate's `include` keeps its package to `src/` and the license texts.
Its unit tests in `src/` use `snapshot-golden`, and `mc683xx-run`'s
tests `mc683xx-examples`, both path-only dev-dependencies that `cargo
publish` drops, so `cargo test` in an unpacked package fails to
build.  The tests run from the repository.

Each published crate and `crates/cpu32-py` holds copies of the root's
`LICENSE-MIT` and `LICENSE-APACHE`, so its package, the wheels and the
sdist carry them (`license-files` in `pyproject.toml`).  They are
copies, not links, for checkouts on Windows; `make licenses`, part of
`make ci` and of CI, checks that they match the root's byte for byte.
A change to a license text is made at the root and copied.

The wheels are abi3 (Python 3.10 and later), one per platform: Linux
manylinux x86_64 and aarch64, macOS x86_64 and arm64, Windows x64, and
an sdist.  Wheels the runner can load run the Python tests on Python
3.10 first; the cross-built ones (Linux aarch64, macOS x86_64) are only
built.  The package is a maturin `python-source` layout
(`crates/cpu32-py/python/mc683xx`), so the sdist carries the stubs and
`py.typed`; the `sdist` job builds a wheel from the sdist, as pip does
where no wheel fits, and checks they are in it.  `mc683xx-run`
installs on any host; its `nix` dependency, for `--bind NAME=pty`, is
Unix's, and elsewhere that binding is refused.

## Versions and tags

- Semantic versioning from 0.1.0.  Before 1.0, a breaking change to a
  public API (Rust or Python) bumps the minor version; anything else
  the patch.
- A release is an annotated tag `vX.Y.Z` on a commit of `main` that
  has passed every check (AGENTS.md).  Its notes, breaking changes
  first, are its GitHub release's.
- A consumer pins a release, or a commit for work not yet released.

## Setup

Once, before the first release.

1. PyPI: add a pending trusted publisher for the project `mc683xx`:
   owner `charlieh0tel`, repository `mc683xx`, workflow `release.yml`,
   environment `pypi`.  Until it exists, the `pypi` job fails and
   nothing is published.
2. GitHub (Settings, Environments): create the environments `pypi`
   and `crates-io`, with no required reviewers.  For each, under
   deployment branches and tags choose selected branches and tags and
   add a tag rule `v*`, so only a `v` tag's run can deploy to it.
3. crates.io: trusted publishing can only be set up for a crate that
   already exists, so the first release is published by hand (Cutting
   a release, step 8), with an API token (`publish-new` scope) in
   `cargo login`, revoked after.  Then, for each published crate, add
   a trusted publisher on crates.io: owner `charlieh0tel`, repository
   `mc683xx`, workflow `release.yml`, environment `crates-io`.  Every
   later release is published by the workflow only.  A crate new to
   the workspace is the same: the first release that has it is
   published by hand, the workspace as at the first release (Cutting
   a release, step 8), with a token that has `publish-update` as well
   as `publish-new`, since the crates already published go up by hand
   with it, and the new crate's trusted publisher added after.

## Cutting a release

1. Pick the version from the changes since the last release (`git log
   vX.Y.Z..main`): a breaking change bumps the minor version (before
   1.0), otherwise the patch.
2. On a branch, bump the version in the root `Cargo.toml`: the
   `version` in `[workspace.package]` and in each entry of
   `[workspace.dependencies]`, an exact `=X.Y.Z`: the crates depend on
   each other at one release, so a host pinning one gets the others
   that were tested with it.  `cargo check --workspace` updates
   `Cargo.lock`.  The wheel's version follows the workspace's.
3. Draft the release notes: what changed for a user, breaking changes
   first.
4. Run every check (AGENTS.md): `make ci`, `python`, `oracle`,
   `singlestep`, `corpus`, `boot-smoke`, `boot-diff` and
   `smartclock-boards`, and `cargo publish --workspace --dry-run`.
5. Commit `Cargo.toml` and `Cargo.lock` as "Release X.Y.Z", and merge
   it to `main`.
6. Wait for `ci` to pass on the merged commit, then tag it `vX.Y.Z`,
   annotated, and push the tag.
7. The tag runs `release`.  Its `gate` job checks that the tagged
   commit is on `main` (an ancestor of `origin/main`), that the tag
   names `Cargo.toml`'s version, and, through the GitHub API with the
   workflow's token (`actions: read`), that the latest `ci` run of
   that commit's push to `main` completed with success; no run, a run
   still going, or a failed or cancelled one fails it, and a re-run
   that passes clears it.  Alongside, the wheels, the sdist
   and its wheel's stub check build.  The `pypi` job uploads only once
   `gate` and the builds have passed.  A tag whose `gate` failed for a
   `ci` run still going is re-run (Actions, Re-run jobs) once `ci`
   has passed.  To look over the artifacts first, make a manual run
   of `release` before tagging: a manual run (Actions, Run workflow)
   skips `gate` and builds the same artifacts without publishing, to
   check a branch.
8. The `crates` job, after `gate` and `pypi`, runs `cargo publish
   --workspace` with a trusted-publishing token.
   A release that adds a crate, the first release among them: that
   job fails, as a trusted
   publishing token cannot create a crate, and it publishes nothing,
   cargo checking every package first.  Publish by hand from a clean
   checkout of the tag.  Each crate already published takes new
   versions by trusted publishing only, so first turn off "require
   trusted publishing" in each one's crates.io settings (Trusted
   Publishing), and turn it back on after.  Cargo verifies each package by building it against the
   registry's copies of the crates before it, and a copy left unpacked
   in `~/.cargo/registry/src` by an earlier attempt is taken as it is,
   so start from none: remove `~/.cargo/registry/src/*/mc683xx-*`, or
   use a fresh `CARGO_HOME`, as here:

   ```
   export CARGO_HOME="$(mktemp -d)"
   cargo login            # publish-new, and publish-update if any crate exists
   cargo publish --workspace --locked
   ```

   Cargo uploads in dependency order, waiting for each crate to reach
   the index: `mc683xx-target`, `mc683xx-cpu32`, `mc683xx-modules`, `mc683xx-mc68331`,
   `mc683xx-board`, `mc683xx-debug`, `mc683xx-gdb`,
   `mc683xx-peripherals`, `mc683xx-run`.  If it stops partway, go on
   with `--exclude` for each crate already up (`cargo publish
   --workspace --exclude mc683xx-target --locked`): a real publish
   refuses a version already on crates.io before uploading anything
   (a dry run only warns), and a crate's dependents need it published
   first.  Then revoke the token, turn trusted publishing only back on,
   and add the new crates' trusted publishers (Setup, step 3).
9. Make a GitHub release from the tag with the notes.

A broken release is yanked (`cargo yank`, PyPI's yank) and fixed in
the next patch version; a version is never published twice.
