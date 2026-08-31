#!/usr/bin/env node
'use strict'

/**
 * Fetches the prebuilt native addon for this host from the GitHub release that
 * matches this package's version.
 *
 * npm carries one `xirr-rs` package; the seven `.node` binaries live on the
 * release instead, so an install pulls exactly the one it can run.
 *
 * Optional escape hatches:
 *   XIRR_RS_BINARY_HOST          base URL to fetch from instead of GitHub
 *   XIRR_RS_SKIP_DOWNLOAD        skip entirely; you supply the .node yourself
 *   XIRR_RS_ALLOW_UNVERIFIED     install without checksum verification
 *   NAPI_RS_NATIVE_LIBRARY_PATH  load an explicit .node; nothing to download
 *
 * Exit codes carry meaning. A supported platform that fails to download exits
 * 1, because the package cannot work. An *unsupported* platform exits 0 with a
 * warning, so lockfile refreshes and cross-platform CI installs still succeed;
 * require() then raises the readable error from binding.js.
 */

const fs = require('node:fs')
const path = require('node:path')
const http = require('node:http')
const https = require('node:https')
const crypto = require('node:crypto')

const { BINARY_NAME, REPO, SUPPORTED, binaryFileName } = require('./platform.js')
const { version } = require('./package.json')

const DOWNLOAD_TIMEOUT_MS = 60_000
const ATTEMPTS = 3

function assetUrl(file) {
  const host = process.env.XIRR_RS_BINARY_HOST || process.env.npm_config_xirr_rs_binary_host
  if (host) {
    return `${host.replace(/\/+$/, '')}/${file}`
  }
  return `https://github.com/${REPO}/releases/download/v${version}/${file}`
}

/** GET with redirect following. Resolves to a Buffer. */
function fetchBuffer(url, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    // An internal mirror may well be plain HTTP, and GitHub redirects across
    // hosts, so pick the client from the URL rather than assuming https.
    const client = new URL(url).protocol === 'http:' ? http : https

    const request = client.get(url, { headers: { 'user-agent': `${BINARY_NAME}/${version}` } }, (response) => {
      const { statusCode, headers } = response

      if (statusCode >= 300 && statusCode < 400 && headers.location) {
        response.resume()
        if (redirectsLeft === 0) {
          reject(new Error(`too many redirects fetching ${url}`))
          return
        }
        resolve(fetchBuffer(new URL(headers.location, url).toString(), redirectsLeft - 1))
        return
      }

      if (statusCode !== 200) {
        response.resume()
        reject(new Error(`GET ${url} failed with HTTP ${statusCode}`))
        return
      }

      const chunks = []
      response.on('data', (chunk) => chunks.push(chunk))
      response.on('end', () => resolve(Buffer.concat(chunks)))
      response.on('error', reject)
    })

    request.setTimeout(DOWNLOAD_TIMEOUT_MS, () => {
      request.destroy(new Error(`GET ${url} timed out after ${DOWNLOAD_TIMEOUT_MS}ms`))
    })
    request.on('error', reject)
  })
}

async function fetchWithRetry(url) {
  let lastError
  for (let attempt = 1; attempt <= ATTEMPTS; attempt++) {
    try {
      return await fetchBuffer(url)
    } catch (error) {
      lastError = error
      if (attempt < ATTEMPTS) {
        const backoff = 500 * 2 ** (attempt - 1)
        console.warn(`[${BINARY_NAME}] attempt ${attempt} failed (${error.message}), retrying in ${backoff}ms`)
        await new Promise((resolve) => setTimeout(resolve, backoff))
      }
    }
  }
  throw lastError
}

/**
 * Expected sha256 for `file`, from the checksums.json that the release
 * workflow writes into the tarball. Absent only in a locally built package.
 */
function expectedChecksum(file) {
  const manifest = path.join(__dirname, 'checksums.json')
  if (!fs.existsSync(manifest)) {
    return null
  }
  try {
    return JSON.parse(fs.readFileSync(manifest, 'utf8'))[file] ?? null
  } catch {
    return null
  }
}

async function main() {
  if (process.env.XIRR_RS_SKIP_DOWNLOAD || process.env.npm_config_xirr_rs_skip_download) {
    console.log(`[${BINARY_NAME}] XIRR_RS_SKIP_DOWNLOAD set, skipping download`)
    return
  }

  if (process.env.NAPI_RS_NATIVE_LIBRARY_PATH) {
    console.log(`[${BINARY_NAME}] NAPI_RS_NATIVE_LIBRARY_PATH set, skipping download`)
    return
  }

  // Working from a clone: `napi build` produces the .node, not this script.
  if (fs.existsSync(path.join(__dirname, 'Cargo.toml'))) {
    return
  }

  const file = binaryFileName()
  if (!file) {
    console.warn(
      `[${BINARY_NAME}] no prebuilt binary for ${process.platform}-${process.arch}; ` +
        `supported: ${SUPPORTED.join(', ')}`,
    )
    return
  }

  const destination = path.join(__dirname, file)
  if (fs.existsSync(destination)) {
    return
  }

  const url = assetUrl(file)
  console.log(`[${BINARY_NAME}] downloading ${file}`)

  const binary = await fetchWithRetry(url)

  const expected = expectedChecksum(file)
  if (expected) {
    const actual = crypto.createHash('sha256').update(binary).digest('hex')
    if (actual !== expected) {
      throw new Error(`checksum mismatch for ${file}\n  expected ${expected}\n  received ${actual}`)
    }
  } else if (!process.env.XIRR_RS_ALLOW_UNVERIFIED) {
    throw new Error(
      `checksums.json is missing from this package, so ${file} cannot be verified.\n` +
        '  This should not happen in a published release; please report it.\n' +
        '  Set XIRR_RS_ALLOW_UNVERIFIED=1 to install without verification.',
    )
  }

  // Write to a temp name and rename, so an interrupted install cannot leave a
  // half-written .node behind that later looks like a usable cached binary.
  const temporary = `${destination}.${process.pid}.tmp`
  fs.writeFileSync(temporary, binary, { mode: 0o755 })
  fs.renameSync(temporary, destination)

  console.log(`[${BINARY_NAME}] installed ${file} (${binary.length} bytes)`)
}

main().catch((error) => {
  console.error(
    [
      '',
      `  ${BINARY_NAME} could not download its native binary.`,
      '',
      `  ${error.message}`,
      '',
      '  Behind a proxy or firewall? Point the installer at a mirror of the',
      '  release assets:',
      '',
      `      npm config set xirr_rs_binary_host https://your-mirror/xirr-rs/v${version}`,
      '',
      '  Or fetch the binary by hand and load it directly:',
      '',
      `      https://github.com/${REPO}/releases/tag/v${version}`,
      '      export NAPI_RS_NATIVE_LIBRARY_PATH=/path/to/xirr-rs.<platform>.node',
      '',
    ].join('\n'),
  )
  process.exit(1)
})
