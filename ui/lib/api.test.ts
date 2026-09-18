import { describe, expect, it } from 'vitest';
import { selectBackendAsset } from './api';

const assets = [
    { name: 'nscb_rust.exe', browser_download_url: 'windows' },
    { name: 'nscb_rust-linux-amd64', browser_download_url: 'linux' },
    { name: 'nscb_rust-macos-arm64', browser_download_url: 'arm' },
    { name: 'nscb_rust-macos-amd64', browser_download_url: 'intel' },
];

describe('selectBackendAsset', () => {
    it.each([
        ['windows', 'windows'],
        ['linux', 'linux'],
        ['macos-arm64', 'arm'],
        ['macos-amd64', 'intel'],
    ])('selects the exact backend for %s', (platform, expectedUrl) => {
        expect(selectBackendAsset(platform, assets)?.browser_download_url).toBe(expectedUrl);
    });

    it('does not fall back to the wrong macOS architecture', () => {
        expect(selectBackendAsset('macos-amd64', assets.filter((asset) => asset.name !== 'nscb_rust-macos-amd64'))).toBeUndefined();
    });
});
