# tcloc

A performant TUI that renders a live treemap of your codebase, sized by lines
of code and colored by language. Inspired by
[cloc](https://github.com/AlDanial/cloc).

![demo](assets/demo.gif)

![files view](assets/files.webp)

![nested view](assets/nested.webp)

## Install

Install the latest release from [crates.io](https://crates.io/crates/tcloc)
with [Rust and Cargo](https://rustup.rs/):

```bash
cargo install tcloc --locked
```

Or build from source:

```bash
git clone https://github.com/leonardcser/tcloc.git
cd tcloc
cargo install --path . --locked
```

## Releasing

Releases are published to crates.io by `.github/workflows/publish.yml` using
Trusted Publishing (OIDC), without a long-lived API token in GitHub.

For a new crate, crates.io requires the first release to be published manually
with an API token. After committing these changes, create a short-lived crates.io
API token with permission to publish `tcloc`, authenticate locally with
`cargo login`, and run `cargo publish --locked`. Revoke the token afterward.
Do not add it to the repository or GitHub secrets.

After the first publication, open the crate's **Settings > Trusted Publishing**
on crates.io and add a GitHub publisher with:

- Repository owner: `leonardcser`
- Repository name: `tcloc`
- Workflow filename: `publish.yml`
- Environment: leave blank

No GitHub repository secrets are required. The workflow requests a temporary
crates.io token using GitHub's OIDC identity.

For subsequent releases, update the version in `Cargo.toml`, refresh `Cargo.lock` with
`cargo check`, and commit both files. Push a matching tag, such as `v0.1.0`.
The workflow verifies the tag matches the package version, checks formatting,
runs Clippy and tests, verifies the package, and publishes it to crates.io.

## Usage

```bash
tcloc [PATH]
```

Scan the current directory:

```bash
tcloc
```

Scan only files tracked by git:

```bash
tcloc --vcs git
```

## Views

`Tab` cycles between three views:

- **tree** — folders and files at the current depth.
- **files** — every file flattened, regardless of depth.
- **nested** — full hierarchy at once. Each folder is a darker container of its
  dominant language; its children sit inside it, recursively.

## Navigation

| Input                     | Action                              |
| ------------------------- | ----------------------------------- |
| `hjkl` / arrow keys       | Move selection                      |
| `Enter`                   | Zoom into the selected folder       |
| `Esc` / `Backspace`       | Go up one level                     |
| `Tab`                     | Cycle view                          |
| `o`                       | Open the selected file in `$EDITOR` |
| Left-click a folder       | Zoom in                             |
| Right-click               | Go up                               |
| Mouse wheel on legend     | Scroll the language list            |
| `q` / `Ctrl-C` / `Ctrl-D` | Quit                                |

## Options

| Flag                       | Description                                                      |
| -------------------------- | ---------------------------------------------------------------- |
| `--vcs <VCS>`              | Use a VCS to enumerate files (only `git` supported)              |
| `-j, --threads <N>`        | Worker threads (default: logical CPUs)                           |
| `--max-file-size <MB>`     | Skip files larger than N MB (default: 100)                       |
| `--exclude-dir <NAMES>`    | Comma-separated directory names to skip                          |
| `--include-dir <NAMES>`    | Comma-separated top-level directory names to include             |
| `--exclude-ext <EXTS>`     | Comma-separated file extensions to exclude                       |
| `--include-ext <EXTS>`     | Comma-separated file extensions to include                       |
| `--exclude-lang <LANGS>`   | Comma-separated languages to exclude                             |
| `--include-lang <LANGS>`   | Comma-separated languages to include                             |
| `-H, --hidden`             | Include hidden files and directories                             |
| `-I, --no-ignore`          | Do not honor `.gitignore` / `.ignore`                            |
| `-L, --follow-links`       | Follow symbolic links                                            |
| `-w, --watch`              | Watch the scan root and apply incremental updates on file change |
| `-b, --bench`              | Show a live performance HUD and print a benchmark report on exit |
| `--auto-exit-ms <MS>`      | Exit N ms after the scan finishes (useful with `--bench`)        |

Run `tcloc --help` for the full list.
