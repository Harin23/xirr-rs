# Releasing

## How distribution works

Eight packages go to npm: `xirr-rs`, plus one per platform.

```
xirr-rs                     the JS entry point, no binary
xirr-rs-darwin-arm64        one .node file
xirr-rs-darwin-x64
xirr-rs-linux-arm64-gnu
xirr-rs-linux-arm64-musl
xirr-rs-linux-x64-gnu
xirr-rs-linux-x64-musl
xirr-rs-win32-x64-msvc
```

`xirr-rs` lists all seven as `optionalDependencies`. Each platform package
declares the `os`, `cpu` and `libc` it runs on, so npm installs the one that
matches and skips the rest. There is no install script and nothing is
downloaded from outside the registry.

You never write those packages by hand. `napi create-npm-dirs` generates them
from the `napi.targets` list in `package.json`, which is why `npm/` is
gitignored.

## Cutting a release

You do not. You merge PRs, and a bot proposes the release.

1. Merge PRs to `main` with conventional titles — `feat:`, `fix:`, `perf:`.
   CI rejects a PR title it cannot parse.
2. release-please keeps a PR open titled `chore: release x.y.z`, with the
   version bump and the `CHANGELOG.md` entry already written. It updates itself
   as more work lands.
3. When you want to ship, merge that PR.

Merging it tags the commit, creates the GitHub release with the changelog as
its body, builds all seven targets, and publishes all eight npm packages.

**You never type a version number, and you never create a tag by hand.**

`feat:` gives a minor bump, `fix:` and `perf:` a patch. A breaking change is
`feat!:` or a `BREAKING CHANGE:` footer, and gives a major.

### Releasing an exact version

To force a specific version — a release candidate, or skipping ahead — push a
commit to `main` whose message carries a `Release-As:` footer:

```
chore: cut 0.3.0-rc.1

Release-As: 0.3.0-rc.1
```

The next release PR proposes exactly that version.

A version with a hyphen is published under the `next` dist-tag, so it never
becomes the default install. Use one to exercise the pipeline before a real
release.

> On squash merge GitHub puts the PR *description* into the commit body, so a
> footer there works — but it is easy to lose if you edit the squash body in
> the merge dialog. An empty commit pushed straight to `main` is more reliable.

### When a release fails partway

`napi prepublish` publishes the platform packages one at a time, so a failure
in the middle can leave some published and some not.

**Use "Re-run failed jobs" first.** npm reports an already-published version as
an error, which napi treats as "done, skip" — and the root `xirr-rs` package
publishes last, so a re-run picks up where it stopped. It also reuses the same
build artifacts and the same release-please outputs.

Only cut a new version if something that *did* publish is actually broken. npm
version numbers can never be reused, so a bad one is spent either way.

## Requirements

- `NPM_TOKEN` — repository secret. It must be able to **create new packages**,
  not just publish to existing ones. A granular token scoped to `xirr-rs` alone
  will fail on the platform packages.
- Nothing else; the GitHub release uses the built-in `GITHUB_TOKEN`.
- **Settings → Actions → General → "Allow GitHub Actions to create and approve
  pull requests" must be enabled**, or release-please cannot open its PR at all.

npm caps write tokens at 90 days, so this one will expire and releases will
start failing on a schedule. The permanent fix is trusted publishing, which has
to be configured per package and therefore only after all eight exist:

1. Do the first release with the token.
2. Enable trusted publishing (workflow `release.yml`) on all eight packages.
3. Delete `NPM_TOKEN` **and** remove `registry-url` from `setup-node` in
   `release.yml` — with `registry-url` set and no token, npm skips OIDC
   entirely and publishing fails.

## Versions

`package.json` is the only file with a real version number.

The `Cargo.toml` files are pinned at `0.0.0` on purpose. The crates are never
published to crates.io, so a version there would be decoration that drifts. Do
not "fix" them — nothing reads them.

## Adding a platform

1. Add the Rust target to `napi.targets` in `package.json`.
2. Add a matrix entry to `.github/workflows/build.yml`.

That is all. The platform package is generated from the target name, and npm
works out who should receive it.

## Two things that would break this

**Never run `npm publish --dry-run` in this repo.** `--dry-run` still executes
the `prepublishOnly` lifecycle script, which is `napi prepublish` — and that
really publishes the seven platform packages. It is not a preview. To inspect
what would ship, use `npm pack --dry-run --ignore-scripts`.

**Do not add `--locked` or `--frozen` to any `cargo` step in CI.** Nothing bumps
`Cargo.lock` any more, so it is allowed to sit slightly behind; a locked build
would start failing the first time a dependency moves.
