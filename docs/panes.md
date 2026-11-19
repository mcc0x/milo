# Using the panes

Run the panes in a Unix terminal with 256 colours. Outline and history start in the current directory unless you give them a path. Inside a Git repository they use its root, even when you start in a subfolder. In a plain folder, outline shows just the files and history says it is not a Git project. To outline a directory named like a command, give its path explicitly: `milo ./history`.

## What fits in a pane

| Pane | Size | What you see |
| --- | --- | --- |
| Outline | 7 rows or fewer | A strip of changed files in a Git project, most recently written first, flowed across the width |
| Outline | 52 columns or more | In the tree, the type of change and how many lines each file gained and lost |
| Outline and history | 12 rows or more | A key hint |
| History | 3 rows or fewer | Recent commits without the heading, dots or divider |
| History | 52 columns and 8 rows or more | A row of dots when there is more than one commit |
| History | Narrow | A shorter summary, such as `↑2` for two commits to push or `no tracking` for a branch that does not track a remote branch |

The outline and history update on their own. macOS uses file-change notifications. Where the pane needs to watch but notifications are missing, including on Linux, milo checks about every two seconds. See [keys and marks](keys.md) for colours and symbols.

## Comparing commits

History counts commits to push against the branch's tracked remote branch. `no branch tracked` means there is no tracked branch, not that all commits have been pushed. Milo can still use the remote's default branch to mark commits for comparison. `no remote` means the project has no remote configured.

A shallow clone has only part of the project's history. Milo lists the commits Git has, but it cannot list a commit's changed files if its parent is missing. In that case history shows `Git did not answer for this commit.` instead of showing every file as new.

## Reading files

Open a file from the outline or history with `Enter` or a double-click. Inside Rex it opens in a floating reader, leaving the original pane in place. Elsewhere the reader takes over that pane; `q` brings you back.

`milo view FILE` reads a working file. `milo view FILE --commit ID` reads the version from a commit. For the latter, start at the repository root and use the file's path as it was in that commit. Use a full or abbreviated hexadecimal commit ID, not a branch name or `HEAD`.

The reader shows the whole file with changed lines marked. It colours Rust, Python, JavaScript, TypeScript (including JSX and TSX), Go, Java, C, C++, C#, PHP, shell, JSON, YAML and TOML. It uses a small scanner, not a full parser. Other languages show as plain text. The working-file reader will not open a file outside the project through a linked folder.

## Reading pictures

The working-file reader can show PNG, JPEG, GIF, WebP, HEIC and HEIF, TIFF, BMP, ICO and ICNS, SVG, PSD and the first page of a PDF, where conversion is supported. It fits the picture without changing its proportions. On macOS, `/usr/bin/sips` converts other formats and resizes large PNGs; other systems show only PNGs that do not need conversion. Ordinary text still shows as text if a file has a picture extension. At a commit, the reader shows the stored file as text or a note, not as a picture.

Pictures need a terminal that supports the kitty graphics protocol, such as Rex. Milo asks the terminal whether it can draw pictures and shows a message if it cannot.
