// Fails if the generated loader has no branch for a target we publish.
//
// 0.2.0 shipped an index.js with no win32 branch while x86_64-pc-windows-msvc
// was in napi.targets, so Windows installs threw "Cannot find native binding"
// with xirr-rs.win32-x64-msvc.node sitting unread beside the loader. The
// per-platform binding tests catch that too, but only once the whole matrix
// has built; this fails in seconds and names the missing platform.

import { readFileSync } from 'node:fs'

// napi names each branch by its platformArchABI rather than by the Rust
// triple. Every mapping below was read off a generated index.js, not recalled.
// An unmapped target is a hard error: a triple nobody has mapped is precisely
// the case this check exists to notice.
const TARGET_TO_ABI = {
  'aarch64-apple-darwin': 'darwin-arm64',
  'x86_64-apple-darwin': 'darwin-x64',
  'universal-apple-darwin': 'darwin-universal',
  'x86_64-pc-windows-msvc': 'win32-x64-msvc',
  'i686-pc-windows-msvc': 'win32-ia32-msvc',
  'aarch64-pc-windows-msvc': 'win32-arm64-msvc',
  'x86_64-unknown-linux-gnu': 'linux-x64-gnu',
  'x86_64-unknown-linux-musl': 'linux-x64-musl',
  'aarch64-unknown-linux-gnu': 'linux-arm64-gnu',
  'aarch64-unknown-linux-musl': 'linux-arm64-musl',
  'armv7-unknown-linux-gnueabihf': 'linux-arm-gnueabihf',
  'armv7-unknown-linux-musleabihf': 'linux-arm-musleabihf',
  'riscv64gc-unknown-linux-gnu': 'linux-riscv64-gnu',
  'riscv64gc-unknown-linux-musl': 'linux-riscv64-musl',
  'loongarch64-unknown-linux-gnu': 'linux-loong64-gnu',
  'loongarch64-unknown-linux-musl': 'linux-loong64-musl',
  'powerpc64le-unknown-linux-gnu': 'linux-ppc64-gnu',
  's390x-unknown-linux-gnu': 'linux-s390x-gnu',
  'aarch64-linux-android': 'android-arm64',
  'armv7-linux-androideabi': 'android-arm-eabi',
  'x86_64-unknown-freebsd': 'freebsd-x64',
  'aarch64-unknown-freebsd': 'freebsd-arm64',
}

const annotate = (message) => {
  console.error(process.env.GITHUB_ACTIONS ? `::error::${message}` : message)
}

const pkg = JSON.parse(readFileSync('package.json', 'utf8'))
const targets = pkg.napi?.targets ?? []

// napi names the sibling packages `${napi.packageName ?? name}-${abi}`, which
// is NOT always `${name}-${abi}`: three of the original `xirr-rs-*` names were
// unpublished in August 2026 and npm rejects republishing a tombstoned name
// (E409 "Failed to save packument"), so the platform packages were renamed to
// `xirr-rs-native-*` while the entry point kept `xirr-rs`. Reading `name` here
// instead would look for requires the loader never contains.
const bindingPackageName = pkg.napi?.packageName ?? pkg.name

if (targets.length === 0) {
  annotate('package.json declares no napi.targets, so there is nothing to check')
  process.exit(1)
}

const loader = readFileSync('index.js', 'utf8')
const problems = []

for (const target of targets) {
  // The loader reaches wasi through xirr-rs.wasi.cjs, not a platform branch.
  if (target.startsWith('wasm32-')) {
    continue
  }

  const abi = TARGET_TO_ABI[target]
  if (!abi) {
    problems.push(`${target}: unmapped triple — add it to TARGET_TO_ABI in scripts/check-binding-targets.mjs`)
    continue
  }

  // Both paths matter: the bundled-binary one for a local build, the sibling
  // package one for what users actually install.
  const missing = [
    `require('./${pkg.napi.binaryName}.${abi}.node')`,
    `require('${bindingPackageName}-${abi}')`,
  ].filter((call) => !loader.includes(call))

  if (missing.length > 0) {
    problems.push(`${target} (${abi}): index.js is missing ${missing.join(' and ')}`)
  }
}

if (problems.length > 0) {
  for (const problem of problems) {
    annotate(problem)
  }
  annotate('Run `pnpm build` to regenerate index.js, then commit it.')
  process.exit(1)
}

console.log(`index.js covers all ${targets.length} published targets`)
