'use strict'

/**
 * Which prebuilt binary this host needs.
 *
 * Shared by postinstall.js (which downloads it) and binding.js (which reports
 * a readable error when it is missing). Both must agree, or we would fetch one
 * file and then look for another.
 *
 * The musl probe mirrors the one in the generated index.js, in the same order.
 */

const fs = require('node:fs')
const { execSync } = require('node:child_process')

const REPO = 'Harin23/xirr-rs'
const BINARY_NAME = 'xirr-rs'

/** Targets built by .github/workflows/build.yml. Keep the two in step. */
const SUPPORTED = [
  'darwin-arm64',
  'darwin-x64',
  'linux-arm64-gnu',
  'linux-arm64-musl',
  'linux-x64-gnu',
  'linux-x64-musl',
  'win32-x64-msvc',
]

const isFileMusl = (f) => f.includes('libc.musl-') || f.includes('ld-musl-')

const isMuslFromFilesystem = () => {
  try {
    return fs.readFileSync('/usr/bin/ldd', 'utf-8').includes('musl')
  } catch {
    return null
  }
}

const isMuslFromReport = () => {
  let report = null
  if (typeof process.report?.getReport === 'function') {
    process.report.excludeNetwork = true
    report = process.report.getReport()
  }
  if (!report) {
    return null
  }
  if (report.header && report.header.glibcVersionRuntime) {
    return false
  }
  if (Array.isArray(report.sharedObjects) && report.sharedObjects.some(isFileMusl)) {
    return true
  }
  return false
}

const isMuslFromChildProcess = () => {
  try {
    return execSync('ldd --version', { encoding: 'utf8' }).includes('musl')
  } catch {
    return false
  }
}

function isMusl() {
  if (process.platform !== 'linux') {
    return false
  }
  let musl = isMuslFromFilesystem()
  if (musl === null) {
    musl = isMuslFromReport()
  }
  if (musl === null) {
    musl = isMuslFromChildProcess()
  }
  return musl
}

/** The napi triple for this host, or null if we publish nothing for it. */
function detectTriple() {
  const { platform, arch } = process
  if (platform === 'win32') {
    return arch === 'x64' ? 'win32-x64-msvc' : null
  }
  if (platform === 'darwin') {
    if (arch === 'arm64') return 'darwin-arm64'
    if (arch === 'x64') return 'darwin-x64'
    return null
  }
  if (platform === 'linux') {
    const libc = isMusl() ? 'musl' : 'gnu'
    if (arch === 'x64') return `linux-x64-${libc}`
    if (arch === 'arm64') return `linux-arm64-${libc}`
    return null
  }
  return null
}

/** File name of the addon for this host, or null on an unsupported platform. */
function binaryFileName() {
  const triple = detectTriple()
  return triple ? `${BINARY_NAME}.${triple}.node` : null
}

module.exports = { BINARY_NAME, REPO, SUPPORTED, detectTriple, binaryFileName, isMusl }
