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

```sh
git checkout main && git pull
npm version 0.3.0          # updates package.json, commits, tags v0.3.0
git push --follow-tags
```

Pushing the tag runs `.github/workflows/release.yml`, which builds all seven
targets, runs the tests, then publishes. The publish step refuses to run if the
tag disagrees with `package.json`.

The actual publish is one `npm publish`. That triggers `prepublishOnly`, which
is `napi prepublish -t npm`, and it does three things before the root package
goes out:

1. publishes the seven platform packages;
2. creates the GitHub release and attaches the binaries;
3. writes the platform packages into `optionalDependencies`.

### Prereleases

A version with a hyphen (`0.3.0-rc.1`) is published under the `next` dist-tag,
so it cannot become the default install. Use one to exercise the pipeline
before a real release.

### When a release fails partway

`napi prepublish` publishes the platform packages one at a time, so a failure
in the middle can leave some published and some not. npm version numbers cannot
be reused, so **do not retry the same version** — bump the patch and tag again.

This is the main reason to cut an `-rc` first.

## Requirements

- `NPM_TOKEN` — repository secret, an npm automation token with publish rights.
- Nothing else; the GitHub release uses the built-in `GITHUB_TOKEN`.

## Adding a platform

1. Add the Rust target to `napi.targets` in `package.json`.
2. Add a matrix entry to `.github/workflows/build.yml`.

That is all. The platform package is generated from the target name, and npm
works out who should receive it.
