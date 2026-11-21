# macops

macops is a Rust CLI for managing software, agent profiles, skills, plugins, and configuration on macOS. Use `mo` or `macops` to invoke the same program. macops records identities, recipes, and operation results while native tools maintain their own installations and settings.

## Install

Download the Apple Silicon archive and `SHA256SUMS` from the [GitHub releases](https://github.com/samuelj1519/macops/releases). In the download directory, verify the checksum and extract the archive.

```sh
shasum -a 256 -c SHA256SUMS
tar -xzf macops-v0.2.1-darwin-arm64.tar.gz
```

For a new installation, place the binary in your user directory and create both command names.

```sh
mkdir -p "$HOME/.local/libexec" "$HOME/.local/bin"
install -m 755 macops "$HOME/.local/libexec/macops"
ln -s ../libexec/macops "$HOME/.local/bin/macops"
ln -s ../libexec/macops "$HOME/.local/bin/mo"
mo --version
```

Add `$HOME/.local/bin` to `PATH` if needed.

To build from source, use the Rust version pinned in `rust-toolchain.toml` and run `cargo build --release --locked`. The build produces `target/release/macops`.

## Browse resources

Run `mo` or `mo list` in a terminal. The inventory has application, capability, agent, and configuration tabs. The capability tab includes skills and native plugins. Use `mo help advanced` for the full command and option reference.

`mo apps`, `mo agents`, `mo skills`, and `mo config` remain direct entry points.

| Key | Action |
| --- | --- |
| Tab / Shift+Tab | Switch inventory tabs |
| Arrow keys or `j` / `k` | Move the selection |
| `Ctrl+u` / `Ctrl+d` | Scroll by half a page |
| `/` | Search |
| `Enter` / `Esc` | Open details or go back |
| `q` | Quit |
| `a` in Skills | Toggle discovered-only skills |
| `a` in unified application or capability details | Check whether the selected instance can be adopted |
| `u` / `x` in unified application details | Preview upgrade or removal for an eligible registered npm instance |
| `r` in Config | Refresh the enrolled chezmoi inventory |
| `o` / `s` in Config details | Read the destination or source file |

Skill searches can include an agent such as `@codex`. The configuration viewer opens verified, regular UTF-8 files up to 256 KiB in read-only mode.

Skill details show observed files and the profiles that can discover them through their own, shared, or compatible directories. These are local directory observations. Plugin details in the capability tab show the native registration, payload, and configured enablement separately for each observed Codex or Claude profile. Cursor plugin enablement and session loading remain unverified.

The direct browser shortcuts return a text list without a terminal. Non-interactive `mo list` retains its structured inventory output. Use `--json` for the response envelope and `mo list --kind applications|agents|capabilities|configs` to query a resource category. `--project /absolute/path` selects a project context. Results distinguish discovery, adoption, configured state, and verified loading. The `coverage` field reports inspected sources and incomplete evidence.

### Plugin marketplaces

`mo plugins` lists the native marketplace entry points for Codex, Claude Code, and Cursor. It reads locally registered Codex and Claude Code marketplaces and their observed catalog files. Use `mo plugins claude:mi` to select one Claude profile. Catalog entries describe offers. Installed plugins and their configured state appear separately in the capability tab.

`mo plugins cursor --online` delegates the account marketplace query to the installed Cursor CLI. It uses the native login and may update Cursor's own cache. Use the native `/plugins` or `/plugin` entry shown for your Harness to browse online catalogs and install a new plugin. These actions follow the Harness's own scope and confirmation.

```sh
mo info agent:claude:mi
mo skills list --details
mo discover
mo dirs --roots
```

## Operations and recovery

- `install`, `upgrade`, `uninstall`, and `adopt` operate on a unique target. Use the inventory ID when names overlap. Each provider checks its own source, scope, and installation state.
- Write commands execute by default. Use `--dry-run` to preview without starting an installer or writing operation records. The preview reports an unresolved version when local evidence is insufficient.
- `update` refreshes version indexes. `outdated` reads the existing index. Where the provider requires a specific version, use `--version`.
- `--yes` skips the confirmation prompt and retains the same validation. For AI CLI upgrades, `--force-live` limits the change to the live installation.
- JSON results use exit code 0 for success, 1 for errors, 2 for invalid usage, and 3 for results requiring attention. Native exit status is recorded separately.

Use `mo history` to find an operation ID. `mo recover ID` checks the original operation and its evidence. Operations with unresolved outcomes can block later writes. After verifying the outcome outside macops, `mo recover ID --resolve-failed --yes` records a failure where the provider supports manual resolution.

## Software and installation sources

Homebrew, mise, npm, Bun, Cargo, and explicit command recipes use their native maintenance interfaces within the supported scopes. [uv discovery and adoption](docs/uv-readonly.md) are read-only. Use `mo ai upgrade` for supported AI CLI installations. Service operations such as `mo service start postgresql` require a declared service in a verified user scope.

A version 1 catalog can be selected with `--catalog /absolute/path/catalog.json`. The default is `~/.config/macops/catalog.json` unless a different configuration root is selected. Treat command recipes as executable configuration. Command providers pass program and argument vectors directly. Query commands return an object with `status: "present"` and `version`, `status: "missing"`, or `status: "unknown"` and `reason`.

Homebrew formulae bind to the executable, prefix, Cellar, and full formula name. npm binds to the prefix, package name, package manifest, and applicable npm configuration. You can adopt an npm discovery as a read-only receipt. Declare a matching catalog entry with write authority, then run `mo adopt ID --bind-recipe` to retain the same identity for maintenance.

### Installation advice

```sh
mo recommend jq
mo recommend ayghri/i-have-adhd
mo recommend ripgrep --github BurntSushi/ripgrep
mo recommend jq --model
```

Recommendations can use bundled evidence, a selected public GitHub repository, or a local file supplied with `--evidence /absolute/path/sources.json`. A local evidence document has `schema_version: 1` and a `sources` array. Each source contains `subject`, an HTTPS `url`, and `excerpt`.

GitHub lookup checks a selected README and compares up to six npm or Homebrew core suggestions with publication metadata. Requested exact npm versions are checked individually. Suggestions without a version use the current release. Repository links in publisher metadata are evidence for the comparison. A matching registered recipe is shown with its ID for a native preview.

For a repository name or GitHub repository URL, macops pins a public commit and links to its README and `INSTALL.md` when available. Run `mo setup` in a terminal to configure a DeepSeek or custom HTTPS OpenAI-compatible endpoint. The wizard confirms a connection test and saves the key in Keychain. Repository recommendations use that profile when enabled and interactive. The model receives public document excerpts and tool names. Its structured proposals are advisory.

When a name matches an eligible, registered npm recipe, the interactive recommendation can offer a versioned installation plan. It runs the selected plan after confirmation. Other proposals display their source without granting installation authority.

The explicit `--model` compatibility option asks DeepSeek to explain bundled public evidence. Its generic password has service `org.macops.recommend.deepseek` and account `default`. Use `--keychain-account` to select another account. Dry runs skip model requests.

## Configuration

Use `mo config enroll` to register a chezmoi owner. The `--chezmoi-source`, `--chezmoi-destination`, and `--chezmoi-config` options select a native context. Supplying all three also selects the context for a single eligible native operation.

| Command | Purpose |
| --- | --- |
| `mo config list --refresh` | Refresh the native inventory |
| `mo config source-path [TARGET]` | Find the source root or a target mapping |
| `mo config status [TARGET...]` / `mo config diff [TARGET]` | Inspect configuration differences |
| `mo config apply TARGET` | Apply one verified path or cached configuration ID |
| `mo config verify TARGET...` | Verify selected targets |
| `mo config adopt /absolute/path` | Register one regular file with an enrolled static source |
| `mo config forget ID` | Remove one registered source entry and preserve the destination file |

Source adoption and removal support chezmoi 2.73.3 with a complete inventory, an existing source parent, and the default macops state directory for the system account. Review the dry run before adopting. Adoption creates a plaintext source file.

Explicit native apply and query operations use the owner's chezmoi configuration, including configured hooks, helpers, caches, and network behavior. The coverage result preserves native filtering as partial evidence. Re-enroll after changing the native cache or state path. Recovery checks the original operation for a partial or unknown apply. Native restore requires a forward operation. Legacy local transactions retain their original backup and restore rules.

## Skills and agent profiles

For supported GitHub skills, macops stores a recipe and calls Skills CLI. The recipe specifies the repository, commit, member, client, installation name, and any manual-only or derived content policy.

```sh
mo --dry-run skills install --recipe /absolute/path/recipe.json
mo skills install --recipe /absolute/path/recipe.json
mo skills adopt NAME --recipe /absolute/path/recipe.json
mo skills info NAME
mo skills update NAME
mo skills outdated NAME
mo --dry-run skills upgrade NAME
mo skills remove NAME
```

Member and client selection use native installation arguments. Recipe transformations apply at installation or maintenance. Skills CLI owns the native installation record. macops owns its recipe and derived assets. Choose a unique inventory ID when a name has multiple sources. A batch upgrade records per-item results and lists remaining recipes in `unattempted` when an item fails or has an unknown outcome.

Use `mo skills plugins list` and `mo skills plugins check RECIPE` to inspect plugin evidence. Plugin writes follow the [native plugin contracts](docs/native-plugin-recipes.md). The verified reinstall path covers an enabled local Codex plugin with the same source and version. Claude profiles with different model settings appear as separate entries.

```sh
mo --dry-run skills sync --all
mo --dry-run skills sync --only profile:mi
mo --dry-run skills policy sync
```

Skill or source selectors reconcile links. Profile selectors reconcile the corresponding owned configuration fields. Multiple `--only` options combine selections. `skills policy sync` handles eligible legacy policy entries. Profile synchronization preserves model settings and other fields outside its ownership contract. Use `history` or `skills policy recover` to check original operation evidence.

## Management manifests

```sh
mo --json manifest export | jq .data.manifest > manifest.json
mo manifest plan manifest.json
mo --dry-run manifest replay manifest.json ID
```

Export records identities, scope, recipe digests, and version evidence from local records. Plan compares the manifest with the current scope and labels replay eligibility. Replay executes one explicitly selected npm item through the existing installer or upgrade path. It requires a matching declared recipe and scope, a canonical release or prerelease version without build metadata, and a settled operation history.

## Directories and maintenance

| Data | Default path |
| --- | --- |
| Configuration | `~/.config/macops` |
| Persistent data | `~/.local/share/macops` |
| State and operation history | `~/.local/state/macops` |
| Cache | `~/.cache/macops` |

XDG settings and explicit CLI options can override these paths. Use `mo dirs --roots` to inspect the effective layout. `--home` selects a logical scope. Native execution requires the applicable operating-system isolation when testing with fixtures.

`mo cleanup macops:cache --dry-run` lists owned index files that pass verification. Normal cleanup removes eligible entries without following symlinks. Shared caches and data owned by native managers remain under their respective maintenance contracts.

## Development

macops uses the locked Rust workspace and runs CI on Apple Silicon macOS. Version tags trigger an Apple Silicon release after the checks pass. See the [architecture](docs/architecture.md), [verification guide](docs/verification.md), and [state models](docs/models/README.md).

```sh
git config core.hooksPath .githooks
bash tools/check.sh
```

## License

macops is distributed under the [MIT License](LICENSE).
