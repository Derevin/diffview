# diffview

fzf-driven git diff viewer. Interactive file picker on the left, hunk preview on the right, with toggles for whitespace, side-by-side, target picking (HEAD / merge-base / upstream / `~N` commits back), and a few other knobs.

## Status

Published **as is**. No support, no roadmap, no promises. Bug reports, PRs, and feature requests may go unanswered or be closed without comment. Built and maintained for one person's setup; that may diverge from yours at any time.

Forks welcome.

## Build

```
cargo build --release
```

The binary is `target/release/diffview`. Drop it on your `PATH`.

## Requirements

- `git`
- `fzf`

## Usage

Run `diffview` inside a git repo. Default target is the working tree against `HEAD`.

Single-letter mode flags (any order, before any subcommand):

- `w` — ignore whitespace
- `s` — side-by-side preview
- `x` — exclude files in `/generated` folder
- `m` — diff against merge-base of `origin/main` (or `master`)
- `p` — diff against `@{upstream}`
- `t` — open the target picker
- `~N` — diff `HEAD~N^..HEAD~N`

There is no `--help`. Read the source.

## License

MIT — see [LICENSE](LICENSE).
