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
| History | Narrow | A compact count, such as `↑2` for two commits to push |

The outline and history update on their own. macOS gets file-change notifications; when there is no watch, including on Linux, milo checks about every two seconds. See [keys and marks](keys.md) for what the colours and symbols mean.

## Reading files

Open a file from the outline or history with `Enter` or a double-click. Inside Rex it opens in a floating reader, leaving the original pane in place. Elsewhere the reader takes over that pane; `q` brings you back.

`milo view FILE` reads a working file. `milo view FILE --commit ID` reads the version from a commit. For the latter, start at the repository root and use the file's path as it was in that commit. Use a full or abbreviated hexadecimal commit ID, not a branch name or `HEAD`.

The reader shows the whole file with changed lines marked. It colours Rust, Python, JavaScript, TypeScript (including JSX and TSX), Go, Java, C, C++, C#, PHP, shell, JSON, YAML and TOML. It uses a small scanner, not a full parser: it recognises comments, strings, numbers and reserved words and guesses types and calls from their names.

## Reading pictures

The working-file reader can show PNG, JPEG, GIF, WebP, HEIC and HEIF, TIFF, BMP, ICO and ICNS, SVG, PSD and the first page of a PDF, where conversion is supported. It fits the picture without changing its proportions. For formats other than PNG, and for large PNGs, conversion uses `sips` on macOS; other systems show only PNGs that do not need conversion. At a commit, the reader shows the stored file as text or a note rather than rendering it as a picture.

Pictures need a terminal that supports the kitty graphics protocol, such as kitty, Ghostty, WezTerm or Rex. Milo asks the terminal whether it can draw pictures and shows a message if it cannot.
