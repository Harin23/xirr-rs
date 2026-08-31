# Releasing

## How distribution works

One package goes to npm: `xirr-rs`. It contains no binaries.

The seven `.node` addons are attached to the GitHub release instead. On install,
`postinstall.js` works out the host triple, downloads only that binary from the
release matching the package version, and checks it against the `checksums.json`
baked into the tarball at publish time.

```
npm publish  ──▶  xirr-rs           (~30 KB, no binaries)
                    │
                    └─ postinstall ──▶ github.com/Harin23/xirr-rs/releases/v0.3.0
                                         xirr-rs.linux-x64-gnu.node   ← only this one
```

If the download cannot run, `binding.js` raises an error at `require()` naming
the package manager in play and the flag that unblocks it.

## Cutting a release

```sh
git checkout main && git pull
npm version 0.3.0          # updates package.json, commits, tags v0.3.0
git push --follow-tags
```

Pushing the tag runs `.github/workflows/release.yml`, which:

1. builds all seven targets and runs the test suite (`build.yml`);
2. refuses to continue if the tag disagrees with `package.json`;
3. creates the GitHub release with the binaries and `SHASUMS256.txt`;
4. installs the packed tarball on seven real platforms and loads the module;
5. publishes to npm with provenance — only if step 4 passed everywhere.

Step 4 is the point of the ordering. The npm package is inert until its binaries
are downloadable, so the release is created first and proven before npm sees it.

### Prereleases

A version with a hyphen (`0.3.0-rc.1`) is marked as a prerelease on GitHub and
published to npm under the `next` dist-tag, leaving `latest` alone.

### Dry run

Run the workflow manually from the Actions tab with `dry_run` checked. It builds
everything and stages a **draft** release, then stops — no verification, no npm.

### Release notes

If `CHANGELOG.md` has a `## [0.3.0]` section, it becomes the release body.
Otherwise GitHub generates notes from the commits. Either way an install snippet
is appended.

## Requirements

- `NPM_TOKEN` — repository secret, an npm automation token with publish rights.
- Nothing else; the release itself uses the built-in `GITHUB_TOKEN`.

## When a release fails partway

The workflow is re-runnable. If it dies after creating the release, re-running
updates that release and re-uploads assets with `--clobber` rather than failing
on "already exists".

If npm publish is the step that failed, the GitHub release is already correct —
re-run the workflow, or publish by hand from a checkout of the tag.

## Adding a platform

1. Add the Rust target to `napi.targets` in `package.json`.
2. Add a matrix entry to `.github/workflows/build.yml`.
3. Add the triple to `SUPPORTED` in `platform.js`, and teach `detectTriple()`
   how to recognise the host.
4. Add it to the target checklist in `release.yml` and to a verify matrix.

`platform.js` is shared by the downloader and the loader precisely so those two
cannot drift apart; keep the musl probe in it aligned with the generated
`index.js`, which is where the runtime makes the same decision.
