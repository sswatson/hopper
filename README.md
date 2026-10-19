# Hopper - A TUI for S3

A fast, keyboard-driven terminal UI for browsing and managing S3 buckets.

## Features

- **Configured bucket list** - List every bucket you use in `~/.config/hopper/config.toml`, each with its own AWS profile, starting prefix, and label. `ListBuckets` results are merged in too, but you no longer depend on them (they miss cross-account and shared buckets).
- **Per-bucket credentials and regions** - Buckets from different accounts sit side by side. Each bucket's region is detected automatically.
- **Miller Columns Navigation** - Multi-pane hierarchical browsing with a sliding window, plus a preview pane
- **Object preview** - Metadata (size, storage class, content type, encryption, user metadata) and the first 16 KiB of text objects. Parquet/gzip/zip/etc. are recognised as binary.
- **Edit in `$EDITOR`** - Enter on an object opens it locally; it's uploaded back only if you changed it
- **Upload/Download** - Files or whole prefixes, 8 transfers in parallel, with Tab completion for local paths. Existing files/objects are never overwritten (they are skipped and reported).
- **Rename, move (cut/paste), delete, create folders** - Prefixes are handled recursively
- **Prefix size** - `z` totals the objects and bytes under a prefix
- **Vim-style Keybindings** - hjkl, g/G, Ctrl+u/d, `/` to filter, etc. 

## Installation

```sh
cargo install --path . --locked
# or
just install   # builds release and copies to ~/.local/bin
```

Authentication uses the standard AWS SDK credential chain, so whatever works for the `aws` CLI works here, including SSO profiles (run `aws sso login --profile <name>` if your session has expired).

## Configuration

Put your buckets in `~/.config/hopper/config.toml` (`hopper --print-config-path` shows the exact path). See [config.example.toml](config.example.toml):

```toml
profile = "default"   # optional: profile for buckets that don't name one
discover = true       # also show ListBuckets results (false, or a list of profiles)

buckets = [
  "my-team-bucket",
  "s3://shared-drop/vendor/acme/",                       # opens at a prefix
  { name = "other-account-bucket", profile = "legacy" }, # needs another profile's credentials
  { name = "vendor-feed", profile = "legacy", prefix = "exports/", label = "Vendor exports" },
]
```

You can also manage the list from inside hopper. At the bucket list, `a` opens a form (bucket, profile, prefix, region, label) and appends the entry to the config file, keeping your comments and formatting. `d` removes the selected entries from the config; it never deletes buckets.

A `.config/hopper/config.toml` in the current directory is merged on top of the global file (its buckets are appended).

## Usage

```bash
hopper                                   # configured + discovered buckets
hopper -b my-bucket -b other@legacy      # add buckets for this session (bucket@profile)
hopper -b s3://feed/exports/2024@legacy  # ...optionally at a prefix
hopper --profile dev                     # change the default profile
hopper --no-discover                     # skip ListBuckets
hopper --config ./buckets.toml           # use a specific config file only
hopper --demo                            # mock data, no AWS needed
```

## Keys

| Key | Action |
|---|---|
| `j`/`k`, `↓`/`↑` | Move down/up |
| `h`/`l`, `←`/`→` | Move between panes (slides the window at the edges) |
| `Home`/`Ctrl+g`, `G` | Top / bottom |
| `Ctrl+u` / `Ctrl+d` | Page up / down |
| `Tab` / `Shift+Tab` | Next / previous pane |
| `Enter` | Open bucket or prefix, load the next page, or edit an object in `$EDITOR` |
| `Backspace` / `Esc` | Go back |
| `/` | Filter the active pane (Enter opens the match) |
| `Space` | Mark item (marked items are the targets of `D`, `d`, `x`) |
| `y` | Copy `s3://` URI |
| `o` | Open in the AWS console |
| `D` / `U` | Download selected / upload a local file or directory here |
| `a` | New object (opens `$EDITOR`) or folder (name ending in `/`); at the bucket list: add bucket to config |
| `r` | Rename object or prefix |
| `d` | Delete (with confirmation); at the bucket list: remove from config |
| `x` / `p` | Cut / paste (move) |
| `z` | Compute total size of a prefix or bucket |
| `R` | Refresh |
| `!` | Toggle danger mode (deletes skip confirmation) |
| `[` / `]` | Scroll the preview pane |
| `?` | Help |
| `q` / `Ctrl+C` | Quit |

## Notes and limits

- Single-request uploads and server-side copies are limited to 5 GB per object (no multipart yet).
- Moves are copy-then-delete; a source is deleted only if its copy succeeded. Cross-account moves use the destination bucket's credentials, so those credentials must be able to read the source.
- Objects in `GLACIER`/`DEEP_ARCHIVE` show metadata but can't be previewed or opened until restored.
- Objects larger than 50 MB aren't opened in the editor. Use `D` instead.

## Development

```bash
just demo    # run against the built-in mock store
just test    # unit + action tests (all run against the mock store)
```
