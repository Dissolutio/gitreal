import { describe, expect, it } from 'vitest';
import { checkTauriVersions } from './check-tauri-versions.mjs';

const cargoLock = `[[package]]
name = "tauri"
version = "2.12.0"

[[package]]
name = "tauri-plugin-dialog"
version = "2.8.0"

[[package]]
name = "tauri-plugin-future"
version = "1.4.2"
`;

const packageJson = {
  dependencies: {
    '@tauri-apps/api': '2.12.5',
    '@tauri-apps/plugin-dialog': '2.8.4',
    '@tauri-apps/plugin-future': '1.4.8'
  }
};

const pnpmLock = `importers:
  .:
    dependencies:
      '@tauri-apps/api':
        specifier: 2.12.5
        version: 2.12.5
      '@tauri-apps/plugin-dialog':
        specifier: 2.8.4
        version: 2.8.4
      '@tauri-apps/plugin-future':
        specifier: 1.4.8
        version: 1.4.8

packages: {}
`;

describe('checkTauriVersions', () => {
  it('accepts matching major.minor versions', () => {
    expect(checkTauriVersions({ cargoLock, packageJson, pnpmLock })).toEqual([]);
  });

  it('reports major.minor drift', () => {
    const driftedPackageJson = {
      ...packageJson,
      dependencies: { ...packageJson.dependencies, '@tauri-apps/api': '2.11.5' }
    };

    expect(checkTauriVersions({ cargoLock, packageJson: driftedPackageJson, pnpmLock })).toEqual([
      expect.stringContaining('tauri 2.12.0 does not match @tauri-apps/api 2.11.5')
    ]);
  });

  it('discovers plugin pairs from the lockfiles', () => {
    const driftedLock = pnpmLock.replace('version: 1.4.8', 'version: 1.3.8');

    expect(checkTauriVersions({ cargoLock, packageJson, pnpmLock: driftedLock })).toEqual([
      expect.stringContaining('tauri-plugin-future 1.4.2 does not match @tauri-apps/plugin-future 1.3.8')
    ]);
  });
});
