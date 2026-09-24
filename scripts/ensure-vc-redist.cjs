#!/usr/bin/env node
// Ensures the Visual C++ runtime installer that tauri.windows.conf.json bundles
// as a resource (src-tauri/windows/resources/vc_redist.x64.exe) is present
// before the Tauri app is built.
//
// Why: tauri-build refuses to compile the app when a declared resource is
// missing ("resource path `windows\resources\vc_redist.x64.exe` doesn't
// exist"), and the file is not committed. CI downloads it in a workflow step;
// a fresh clone built locally had no declared way to get it at all.
//
// The download is PINNED: a versioned Microsoft URL (the one the moving
// https://aka.ms/vs/17/release/vc_redist.x64.exe permalink resolved to when
// this was pinned, 14.44.35211.0) plus its SHA-256. A mismatch fails the build
// by name instead of bundling unverified bytes. To move to a newer runtime,
// update URL and SHA256 together in one commit.
//
// Windows only: the resource is declared in tauri.windows.conf.json alone.
const fs = require('fs');
const path = require('path');
const os = require('os');
const cp = require('child_process');
const crypto = require('crypto');

const URL =
  'https://download.visualstudio.microsoft.com/download/pr/bd1c8d9d-ba95-4eee-bc6e-df1fcc876373/CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B/VC_redist.x64.exe';
const SHA256 = 'cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b';
const DEST = path.join(__dirname, '..', 'src-tauri', 'windows', 'resources', 'vc_redist.x64.exe');

function sha256File(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function fail(msg) {
  console.error(`\n[ensure-vc-redist] ${msg}\n`);
  process.exit(1);
}

if (process.platform !== 'win32') {
  console.log('[ensure-vc-redist] not Windows; the VC++ runtime resource is Windows-only, nothing to do.');
  process.exit(0);
}

if (fs.existsSync(DEST)) {
  const have = sha256File(DEST);
  if (have === SHA256) {
    console.log('[ensure-vc-redist] vc_redist.x64.exe present and verified.');
    process.exit(0);
  }
  fail(`${DEST} exists but its SHA-256 is ${have}, not the pinned ${SHA256}. Delete it to re-fetch the pinned file, or update the pin deliberately.`);
}

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'vc-redist-'));
const part = path.join(tmp, 'vc_redist.x64.exe');
try {
  console.log(`[ensure-vc-redist] Downloading ${URL}`);
  try {
    cp.execFileSync('curl', ['--http1.1', '-sS', '-L', '-f', '-o', part, URL], { stdio: 'inherit' });
  } catch (_) {
    cp.execFileSync('powershell', ['-NoProfile', '-Command',
      `[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -UseBasicParsing -Uri '${URL}' -OutFile '${part.replace(/'/g, "''")}'`,
    ], { stdio: 'inherit' });
  }
  const got = sha256File(part);
  if (got !== SHA256) fail(`downloaded file SHA-256 ${got} does not match the pinned ${SHA256}; refusing to bundle it.`);
  fs.mkdirSync(path.dirname(DEST), { recursive: true });
  fs.copyFileSync(part, DEST);
  console.log(`[ensure-vc-redist] Verified SHA-256 ${SHA256}; installed ${path.relative(process.cwd(), DEST)}.`);
} finally {
  try { fs.rmSync(tmp, { recursive: true, force: true }); } catch (_) {}
}
