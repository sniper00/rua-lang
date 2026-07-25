# VS Code Extension Release Guide

## Prerequisites

- Push access to **both** remotes: `origin` (self-hosted) and `github` (`sniper00/rua-lang`)
- `VSCE_PAT` secret configured in GitHub Actions (for Marketplace publishing)
- All CI checks green on `main`

## Step-by-step

### 1. Bump version

Edit `editors/vscode/package.json` and increment `version` (semver):

```json
"version": "<new-version>",
```

### 2. Commit, tag, push

```bash
# Replace with the actual version you're releasing
VERSION="<new-version>"   # e.g. "0.1.3"
git add editors/vscode/package.json
git commit -m "chore: bump extension version to $VERSION"
git tag "v$VERSION"
git push github main         # push commit to GitHub
git push github "v$VERSION"  # push tag to GitHub → triggers vscode-release.yml
```

> **Critical**: the tag must be pushed to **GitHub** (`github` remote), not just `origin`.
> The `vscode-release.yml` workflow only runs on GitHub Actions; pushing tags to the
> self-hosted `origin` will NOT trigger a release.

### 3. Wait for the release workflow

The workflow builds toolchains for 4 platforms, packages the VSIX, and publishes:

```
https://github.com/sniper00/rua-lang/actions/workflows/vscode-release.yml
```

The publish step only runs on tag push (`github.ref_type == 'tag'`). It will:

| Platform    | Binary              |
|-------------|---------------------|
| linux-x64   | `rua-lsp` / `ruac`  |
| linux-arm64 | `rua-lsp` / `ruac`  |
| darwin-arm64| `rua-lsp` / `ruac`  |
| win32-x64   | `rua-lsp.exe` / `ruac.exe` |

### 4. Verify

Check the [Marketplace page](https://marketplace.visualstudio.com/items?itemName=BruceZeros.rua-lang) —
the new version should appear under "Version History" within a few minutes of publish.

## Troubleshooting

| Symptom                              | Likely cause                              |
|--------------------------------------|-------------------------------------------|
| Workflow not triggered               | Tag not pushed to `github` remote         |
| `VSCE_PAT is required` error         | Secret not set in GitHub repo settings    |
| `tag version does not match package` | `package.json` version ≠ tag name (minus `v`) |
| Marketplace rejects duplicate version | Forgot to bump `version` in step 1        |
| npm audit fails                      | Run `cd editors/vscode && npm audit fix`  |
