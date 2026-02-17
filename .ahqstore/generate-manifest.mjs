import { readFileSync, writeFileSync } from 'fs';
import { createHash } from 'crypto';
import { dirname, resolve } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');

// Parse CLI args: --tag vX.Y.Z  --output path  --token TOKEN
function parseArgs() {
  const args = process.argv.slice(2);
  const parsed = { tag: null, output: 'ahqstore.json', token: process.env.GITHUB_TOKEN || null };
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--tag' && args[i + 1]) parsed.tag = args[++i];
    else if (args[i] === '--output' && args[i + 1]) parsed.output = args[++i];
    else if (args[i] === '--token' && args[i + 1]) parsed.token = args[++i];
  }
  return parsed;
}

// Match a release asset using a finder pattern from config.json
function matchAsset(assets, finder) {
  if (!finder) return null;
  return assets.find(a => {
    if (finder.startsWith && !a.name.startsWith(finder.startsWith)) return false;
    if (finder.contains && !a.name.includes(finder.contains)) return false;
    if (finder.endsWith && !a.name.endsWith(finder.endsWith)) return false;
    return true;
  }) || null;
}

const args = parseArgs();

// Load app config
const configPath = resolve(__dirname, 'config.json');
const configRaw = JSON.parse(readFileSync(configPath, 'utf8'));
const appId = Object.keys(configRaw).find(k => k !== '$schema');
const config = configRaw[appId];

// Fetch release from GitHub API
const { author, repo } = config.repo;
const headers = { 'User-Agent': 'ahqstore-manifest-gen' };
if (args.token) headers['Authorization'] = `Bearer ${args.token}`;

const apiUrl = args.tag
  ? `https://api.github.com/repos/${author}/${repo}/releases/tags/${args.tag}`
  : `https://api.github.com/repos/${author}/${repo}/releases/latest`;

const resp = await fetch(apiUrl, { headers });
if (!resp.ok) {
  console.error(`GitHub API error: ${resp.status} ${resp.statusText} (${apiUrl})`);
  process.exit(1);
}

const release = await resp.json();
const releaseText = JSON.stringify(release);

// Compute version hash (SHA256 of release JSON as stringified byte array)
const hash = createHash('sha256').update(releaseText).digest();
const versionHash = JSON.stringify([...hash]);

// Match assets using finder patterns
const assets = release.assets || [];
const winAsset = matchAsset(assets, config.finder.windowsAmd64Finder);
const linuxAsset = matchAsset(assets, config.finder.linuxAmd64Finder);
const androidAsset = matchAsset(assets, config.finder.androidUniversalFinder);

if (!winAsset || !linuxAsset || !androidAsset) {
  const available = assets.map(a => a.name).join(', ');
  console.error('Failed to match all required assets.');
  if (!winAsset) console.error('  Missing: Windows (x64 exe)');
  if (!linuxAsset) console.error('  Missing: Linux (AppImage)');
  if (!androidAsset) console.error('  Missing: Android (APK)');
  console.error(`  Available assets: ${available || '(none)'}`);
  process.exit(1);
}

// Read icon
const iconPath = resolve(repoRoot, 'src-tauri', 'icons', '128x128.png');
const iconBytes = [...readFileSync(iconPath)];

// Build download entries
const downloadUrls = {
  "1": { installerType: config.platform.winAmd64Platform, asset: winAsset.name, url: winAsset.browser_download_url },
  "2": { installerType: config.platform.linuxAmd64Platform, asset: linuxAsset.name, url: linuxAsset.browser_download_url },
  "3": { installerType: config.platform.androidUniversal, asset: androidAsset.name, url: androidAsset.browser_download_url }
};

// Build install config
const install = {
  win32: {
    assetId: 1,
    exec: winAsset.name,
    scope: config.platform.winAmd64Options?.scope ?? null,
    installerArgs: config.platform.winAmd64Options?.exe_installer_args ?? []
  },
  winarm: null,
  linux: { assetId: 2 },
  linuxArm64: null,
  linuxArm7: null,
  android: {
    assetId: 3,
    min_sdk: config.platform.androidOptions?.minSdk ?? 24,
    abi: config.platform.androidOptions?.abi ?? ["Aarch64", "Armv7", "X86", "X64"]
  }
};

// Assemble manifest
const manifest = {
  appId: config.appId,
  appShortcutName: config.appShortcutName,
  appDisplayName: config.appDisplayName,
  authorId: config.authorId,
  releaseTagName: release.tag_name,
  downloadUrls,
  install,
  displayImages: [],
  description: config.description,
  repo: config.repo,
  version: versionHash,
  site: config.site || "https://vectorapp.io",
  source: null,
  license_or_tos: config.license_or_tos || `https://github.com/${author}/${repo}/blob/main/LICENSE`,
  resources: { "0": iconBytes },
  verified: false
};

writeFileSync(args.output, JSON.stringify(manifest));
console.log(`AHQ Store manifest generated: ${args.output}`);
console.log(`  Tag: ${release.tag_name}`);
console.log(`  Windows: ${winAsset.name}`);
console.log(`  Linux:   ${linuxAsset.name}`);
console.log(`  Android: ${androidAsset.name}`);
console.log(`  Icon:    ${iconBytes.length} bytes`);
