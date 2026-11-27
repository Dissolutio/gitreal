import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export function parseCargoLock(lockText) {
  const versions = new Map();

  for (const section of lockText.split(/(?=^\[\[package\]\]$)/m)) {
    const name = section.match(/^name = "([^"]+)"$/m)?.[1];
    const version = section.match(/^version = "([^"]+)"$/m)?.[1];
    if (name && version && (name === 'tauri' || name.startsWith('tauri-plugin-'))) {
      const crateVersions = versions.get(name) ?? [];
      crateVersions.push(version);
      versions.set(name, crateVersions);
    }
  }

  return versions;
}

export function parsePnpmLock(lockText) {
  const lines = lockText.split(/\r?\n/);
  const rootImporter = lines.findIndex((line) => line === '  .:');
  if (rootImporter === -1) {
    throw new Error('Could not find the root importer in pnpm-lock.yaml');
  }

  const versions = new Map();
  let section = null;
  let packageName = null;
  for (const line of lines.slice(rootImporter + 1)) {
    if (/^  [^ ]/.test(line) || /^[^ ]/.test(line)) break;
    const indent = line.match(/^ */)[0].length;

    if (indent === 4) {
      section = /^(dependencies|devDependencies):$/.test(line.trim()) ? line.trim() : null;
      packageName = null;
      continue;
    }

    if (indent === 6 && section) {
      const entry = line.match(/^ {6}(['"]?)(@tauri-apps\/[^'":]+)\1:(?:\s*(.*))?$/);
      packageName = entry?.[2] ?? null;
      if (packageName && entry[3]) {
        const version = entry[3].match(/\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?/)?.[0];
        if (version) versions.set(packageName, version);
      }
      continue;
    }

    if (indent <= 6) {
      packageName = null;
      continue;
    }

    if (indent === 8 && packageName) {
      const version = line.trim().match(/^version:\s*(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)/)?.[1];
      if (version) versions.set(packageName, version);
    }
  }

  return versions;
}

function packageVersions(packageJson) {
  return new Map(
    ['dependencies', 'devDependencies']
      .flatMap((field) => Object.entries(packageJson[field] ?? {}))
      .filter(([name]) => name.startsWith('@tauri-apps/'))
  );
}

function majorMinor(version) {
  return version.match(/^(\d+)\.(\d+)\./)?.slice(1).join('.');
}

export function checkTauriVersions({ cargoLock, packageJson, pnpmLock }) {
  const rustPackages = parseCargoLock(cargoLock);
  const declaredPackages = packageVersions(packageJson);
  const lockedPackages = parsePnpmLock(pnpmLock);
  const pairs = [['tauri', '@tauri-apps/api']];

  for (const crateName of rustPackages.keys()) {
    if (!crateName.startsWith('tauri-plugin-')) continue;
    const packageName = `@tauri-apps/plugin-${crateName.slice('tauri-plugin-'.length)}`;
    if (declaredPackages.has(packageName) || lockedPackages.has(packageName)) {
      pairs.push([crateName, packageName]);
    }
  }

  const errors = [];
  for (const [crateName, packageName] of pairs) {
    const crateVersions = rustPackages.get(crateName) ?? [];
    const declaredVersion = declaredPackages.get(packageName);
    const lockedVersion = lockedPackages.get(packageName);

    if (crateVersions.length === 0 || !declaredVersion || !lockedVersion) {
      errors.push(`Missing version for ${crateName} or ${packageName}`);
      continue;
    }

    for (const crateVersion of crateVersions) {
      const expected = majorMinor(crateVersion);
      if (!expected) {
        errors.push(`Could not parse the version of ${crateName}: ${crateVersion}`);
        continue;
      }

      for (const [source, version] of [['package.json', declaredVersion], ['pnpm-lock.yaml', lockedVersion]]) {
        const actual = majorMinor(version);
        if (!actual) {
          errors.push(`Could not parse the version of ${packageName} in ${source}: ${version}`);
        } else if (actual !== expected) {
          errors.push(
            `${crateName} ${crateVersion} does not match ${packageName} ${version} (${source}) by major.minor`
          );
        }
      }
    }
  }

  return errors;
}

function main() {
  const root = new URL('../', import.meta.url);
  const errors = checkTauriVersions({
    cargoLock: readFileSync(new URL('src-tauri/Cargo.lock', root), 'utf8'),
    packageJson: JSON.parse(readFileSync(new URL('package.json', root), 'utf8')),
    pnpmLock: readFileSync(new URL('pnpm-lock.yaml', root), 'utf8')
  });

  if (errors.length) {
    console.error(errors.join('\n'));
    process.exitCode = 1;
    return;
  }

  console.log('Tauri crate and npm package versions are aligned.');
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main();
}
