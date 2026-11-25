# macops

macops is a Rust CLI for managing software, agent profiles, skills, plugins, and configuration on macOS. Use `mo` or `macops` to invoke the same program. macops records identities, recipes, and operation results while native tools maintain their own installations and settings.

## Install

On an Apple Silicon Mac, run this from the source checkout to build and install the current code. This mode uses the Rust toolchain pinned by the project.

```sh
./install.sh
```

To install the latest [formal release](https://github.com/samuelj1519/macops/releases), run the installer in release mode. It downloads and verifies the archive using the release's SHA-256 checksum. You can use this mode with a standalone copy of `install.sh` and without Rust.

```sh
sh install.sh --release
```

The installer puts the program in `~/.local/libexec/macops` and verifies both `mo` and `macops` in `~/.local/bin`. Rerun the same command to update an existing installation. If installation stops while creating the command links, rerun it to finish. If the terminal cannot find `mo`, follow the PATH instruction printed by the installer.

## Browse resources

See the [operation walkthroughs](docs/operation-flows.md) for software, configuration, skills, Agent profiles, and maintenance.

Run `mo` or `mo app` in a terminal to browse applications, skills and plugins, agent profiles, configuration, and recent operations. Start directly in a category with `mo config`, `mo skill`, or `mo agent`.

Use `mo <type> <target>` for details and `mo <type> <target> <action>` for an operation, for example `mo app jq upgrade --dry-run`. The action-first form `mo app upgrade jq --dry-run` also works. Use a full inventory ID when names are ambiguous. A type prefix can qualify a name, as in `mo app app:jq`. Use `mo jq` or `mo search jq` to look across categories. Run `mo help`, `mo help COMMAND`, or `mo help advanced` to see available arguments.

`mo history` opens recent operations in a terminal. Open an entry awaiting verification or registration and press `c` to confirm recovery of the original operation in its recorded scope. The detail page also shows the operation ID and recovery commands. Explicit target queries and `--json` retain the journal response.

| Key | Action |
| --- | --- |
| Tab / Shift+Tab | Switch inventory tabs |
| Arrow keys or `j` / `k` | Move the selection |
| `Ctrl+u` / `Ctrl+d` | Scroll by half a page |
| `/` | Search |
| `Enter` / `Esc` | Open details or go back |
| `q` | Quit |
| `a` in Skills | Toggle discovered-only skills |
| `a` in software details or the unified capability tab | Check whether the selected instance can be adopted |
| `u` in software details | Preview an upgrade for an eligible npm instance, Homebrew formula, or registered mise tool |
| `x` in software details | Preview removal for an eligible npm instance, Homebrew formula, or inactive mise release |
| `u` / `x` in managed Skills CLI details | Confirm upgrade or removal of the selected native recipe |
| `e` / `l` in managed agent details | Set the plugin preference or launch an independent session |
| `r` in Config | Refresh the enrolled chezmoi inventory |
| `o` / `s` in Config details | Read the destination or source file |

Homebrew actions show the native formula, Cellar, selected versions, and target before confirmation. A registered mise upgrade shows the active and target versions, the installation scope, and the effect on user configuration. Skill lists start with all discovered entries. Searches can include an agent such as `@codex`. The configuration viewer opens verified, regular UTF-8 files up to 256 KiB in read-only mode.

Skill details show observed files and the profiles that can discover them through their own, shared, or compatible directories. These are local directory observations. Plugin details show the native registration, payload, and configured enablement separately for each observed Codex or Claude profile. Cursor project declarations are shown separately from installation and session loading.

Outside a terminal, the category shortcuts print a text list. Use `--json` for structured results. Each category accepts `--project /absolute/path` to select a project in inventory or detail queries. Results distinguish discovery, adoption, configured state, and verified loading. The `coverage` field reports inspected sources and incomplete evidence.

### Plugin marketplaces

`mo skill market` opens a searchable list of Codex, Claude Code, and Cursor marketplaces in a terminal. Press `/` to search by marketplace, profile, or available plugin name, and `Enter` to show the catalog and native entry point. Use `mo skill market claude:mi` to select one Claude profile. Without a terminal the command prints text, and `--json` returns structured results. Installed plugins and their configured state appear in the capability tab.

`mo skill market cursor --online` delegates the account marketplace query to the installed Cursor CLI. It uses the native login and may update Cursor's own cache. Use the native `/plugins` or `/plugin` entry shown for your Harness to browse online catalogs and install a new plugin. These actions follow the Harness's own scope and confirmation.

Use `mo skill --project /absolute/path` to read plugin declarations in a Cursor project's `.cursor/settings.json`. The listing shows the declared setting and source file. Cursor handles installation and loading for that project. See [Cursor's plugin documentation](https://cursor.com/docs/plugins).

For an existing Codex plugin, select its profile-specific capability ID in the inventory and run `mo app adopt ID`. A verified registration and a single identifiable version cache can be recorded as an adoption baseline. The baseline is read-only for native maintenance.

```sh
mo agent agent:claude:mi
mo skill --details
mo app discover
mo dirs --roots
```

## Operations and recovery

- `install`, `upgrade`, `uninstall`, and `adopt` operate on a unique target. Each provider checks its source, scope, and installation state.
- Run `mo app adopt` in a terminal to search installed software that can be adopted. Run `mo app adopt jq` to look up a name directly. When several software entries match, the terminal shows their sources and installation scopes so you can choose. Use `mo app adopt --id ID` for an exact match, including in scripts. The selected target goes through the provider's existing confirmation.
- Write commands execute by default. Use `--dry-run` to preview without starting an installer or writing operation records. The preview reports an unresolved version when local evidence is insufficient.
- `update` refreshes version indexes. `outdated` reads the existing index. Where the provider requires a specific version, use `--version`.
- `--yes` skips the confirmation prompt and retains the same validation. For AI CLI upgrades, `--local` updates the local installation and local mise settings while keeping the saved settings source.
- JSON results use exit code 0 for success, 1 for errors, 2 for invalid usage, and 3 for results requiring attention. Native exit status is recorded separately.

When an earlier operation blocks a change, macops checks its outcome first. If the same request already completed, macops shows that result. If it still needs attention, the message tells you what to check. Browse, search, and previews remain read-only.

Native installers and maintenance commands show their output as it arrives in an interactive terminal. With `--json`, stdout contains the structured result only.

Use `mo history ID` to inspect an operation, `mo recover` to list pending operations, or `mo recover ID` to check one directly. After verifying the outcome outside macops, `mo recover ID --mark-failed` asks for confirmation and records the operation as failed where the provider supports it.

## Software and installation sources

Homebrew, mise, npm, Bun, Cargo, and explicit command recipes use their native maintenance interfaces within the supported scopes. [uv discovery and adoption](docs/uv-readonly.md) are read-only. Use `mo agent install codex VERSION` for the built-in Agent installer. Add `--local` for a supported tool to update the local installation and its mise settings while keeping the saved settings source. Use `mo app upgrade` to refresh the version index and upgrade eligible applications, or add `--dry-run` to preview using the existing index. Service operations such as `mo app start postgresql` require a declared service in a verified user scope.

A version 1 catalog can be selected with `--catalog /absolute/path/catalog.json`. The default is `~/.config/macops/catalog.json` unless a different configuration root is selected. Treat command recipes as executable configuration. Command providers pass program and argument vectors directly. Query commands return an object with `status: "present"` and `version`, `status: "missing"`, or `status: "unknown"` and `reason`.

Homebrew formulae bind to the executable, prefix, Cellar, and full formula name. npm binds to the prefix, package name, package manifest, and applicable npm configuration. You can adopt an npm discovery as a read-only receipt. Declare a matching catalog entry with write authority, then run `mo app adopt ID --bind-recipe` to retain the same identity for maintenance.

mise binds the resolved installation and global configuration paths at adoption. If a tracked mise receipt needs a path review, open its software details and use `a` to confirm the current baseline before upgrading. Removal is available for a tracked tool with a single physical release that mise reports as inactive in the selected user's home. Review any projects that still refer to that version before confirming.

### Installation advice

See the [recommendation and recovery walkthrough](docs/recommend-flow.md) for setup, candidate selection, confirmation, and the next step after each result.

```sh
mo recommend jq
mo recommend ayghri/i-have-adhd
mo recommend ripgrep --github BurntSushi/ripgrep
mo recommend jq --model
```

Recommendations can use bundled evidence, a selected public GitHub repository, or a local file supplied with `--evidence /absolute/path/sources.json`. A local evidence document has `schema_version: 1` and a `sources` array. Each source contains `subject`, an HTTPS `url`, and `excerpt`.

GitHub lookup checks a selected README and compares up to six npm or Homebrew core suggestions with publication metadata. Requested exact npm versions are checked individually. Suggestions without a version use the current release. Repository links in publisher metadata are evidence for the comparison. A matching registered recipe is shown with its ID for a native preview.

For a repository name or GitHub repository URL, macops pins a public commit and links to its README and `INSTALL.md` when available. Run `mo recommend --setup` in a terminal to configure a DeepSeek or custom HTTPS OpenAI-compatible endpoint. The wizard confirms a connection test and saves the key in Keychain. Repository recommendations use that profile when enabled and interactive. The model receives public document excerpts and tool names. Its structured proposals are advisory.

macops uses one managed Keychain item, `org.macops.recommend.api-key`, for the configured recommendation model. Changing only the model on the same service reuses the saved key. To replace the key or switch services, rerun `mo recommend --setup` and confirm the connection test and replacement. The wizard updates the item in place. If a replacement is interrupted, choose `c` in setup to check the original update. Saving a replacement for an older profile also migrates it to the fixed item. Older entries with unverified ownership stay in Keychain. If the new credential is verified and old-entry cleanup is blocked, choose `k` to retain the old item and finish setup.

Saving or recovering recommendation settings uses a per-user lock under `~/.config/macops/recommend-model.lock`. Custom configuration paths share the lock for the same Keychain account. Software maintenance has its own journal and lock. A pending software operation can block installation from `mo recommend` while the setup wizard can still save the model settings.

Daily recommendations request Keychain access silently and continue with local analysis if macOS requires authentication. The explicit setup wizard can ask macOS to grant access to the saved item.

To reference an existing generic password, choose `b` and supply the absolute Keychain file path, service, and account. The managed item stores a reference bound to the selected endpoint. Rebind after changing the external item and manage that item's lifetime in Keychain Access.

When an interactive short-name lookup has no registered install candidate, macops can search public GitHub repositories by that name. Select a source to review its pinned installation evidence.

When a name matches an eligible npm, Homebrew, or mise recipe, the interactive recommendation can offer a versioned installation plan. Homebrew confirms the exact formula, scope, and target version. A registered mise recipe confirms the requested version and its user configuration effect before invoking native global activation. macops checks possible configuration targets against chezmoi ownership before the native command starts.

Eligible Codex and Claude Code marketplace recommendations can install into an existing or new profile after confirmation. The verified Apple Silicon versions are Codex 0.170.0 and Claude Code 2.1.328. The plan binds marketplace and plugin manifests to one public Git commit. At confirmation, enter `yes` to install and enable the plugin or `disabled` to install it and keep it disabled. The result shows the destination profile. Other proposals remain advisory.

If a native stage needs attention, use `mo recover ID` to verify its original result or finish publishing its receipt. Recovery keeps the original operation identity and never starts an installer.

If a Codex or Claude Code stage is verified and a following stage has yet to start, `mo recover ID` reports a continuation command. Use `mo agent resume ID --dry-run` to see the retained profile, source and initial plugin setting, then `mo agent resume ID` to confirm the next stage.

After selecting a Codex or Claude Code plugin in `mo recommend`, choose a profile by name and configuration directory. If a selected on-disk profile needs registration, the installation confirmation includes both steps. Start an existing profile with its usual command. Choose a new independent profile if you want a separate configuration.

In `mo agent`, open a managed independent Codex or Claude Code profile and press `e` to choose the plugin setting for future sessions launched by macops. Press `l` to launch in the current workspace. Each independent profile retains its own native settings, model, and authentication. Use the exact ID in the details view if two profiles share a name.

Use `mo agent launch PROFILE` to launch by name or ID. For Codex, add `--native-profile NAME` to select an existing named configuration in that independent home. `mo agent launch PROFILE --dry-run --json` shows the launch arguments. The launcher uses a system program search path and inherits terminal and locale settings. A plugin that runs a non-system command needs that dependency available in its native profile environment.

The explicit `--model` option uses the configured service to explain bundled public evidence. Without a saved profile, it uses the legacy DeepSeek Keychain service `org.macops.recommend.deepseek` and account `default`. Use `--keychain-account` to select an existing legacy DeepSeek account explicitly. Dry runs skip model requests.

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
| `mo config reconcile [TOOL]` | Report mise version drift across supported AI tools, or reconcile the source for a selected tool |

Source adoption and removal support chezmoi 2.73.3 with a complete inventory, an existing source parent, and the default macops state directory for the system account. Review the dry run before adopting. Adoption creates a plaintext source file.

Explicit native apply and query operations use the owner's chezmoi configuration, including configured hooks, helpers, caches, and network behavior. The coverage result preserves native filtering as partial evidence. Re-enroll after changing the native cache or state path. Recovery checks the original operation for a partial or unknown apply. Native restore requires a forward operation. Legacy local transactions retain their original backup and restore rules.

## Skills and agent profiles

For supported GitHub skills, macops stores a recipe and calls Skills CLI. The recipe specifies the repository, commit, member, client, installation name, and any manual-only or derived content policy.

Use `--recipe` with the absolute path to a local macops recipe JSON file. For a GitHub repository such as a multi-skill kit, run `mo recommend OWNER/REPO` to find installation instructions for your client. When macops cannot build an executable plan, it can show recognized manual commands from the pinned repository documentation or point you to the instructions.

```sh
mo --dry-run skill install --recipe /absolute/path/recipe.json
mo skill install --recipe /absolute/path/recipe.json
mo skill adopt NAME --recipe /absolute/path/recipe.json
mo skill NAME
mo skill refresh NAME
mo skill outdated NAME
mo --dry-run skill upgrade NAME
mo skill uninstall NAME
```

Member and client selection use native installation arguments. Recipe transformations apply at installation or maintenance. Skills CLI owns the native installation record. macops owns its recipe and derived assets. Choose a unique inventory ID when a name has multiple sources. A batch upgrade records per-item results and lists remaining recipes in `unattempted` when an item fails or has an unknown outcome.

Interactive `mo recommend NAME` can offer a matching, uninstalled Skills CLI recipe from the registered recipe book. Confirm the fixed source commit, Codex user scope, and manual-trigger setting. Skills CLI installs to a shared directory that other compatible agents may also discover.

Use `mo skill plugins list` and `mo skill plugins check RECIPE` to inspect plugin evidence. Plugin writes follow the [native plugin contracts](docs/native-plugin-recipes.md). The verified reinstall path covers an enabled local Codex plugin with the same source and version. Claude profiles with different model settings appear as separate entries.

```sh
mo --dry-run skill sync --all
mo --dry-run skill sync --only profile:mi
mo --dry-run skill policy sync
```

Skill or source selectors reconcile links. Profile selectors reconcile the corresponding owned configuration fields. Multiple `--only` options combine selections. `skill policy sync` handles eligible legacy policy entries. Profile synchronization preserves model settings and other fields outside its ownership contract. Use `history` or `skill policy recover` to check original operation evidence.

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
