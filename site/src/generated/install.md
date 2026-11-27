## Install

Use the [v0.1.9 release](https://github.com/mcc0x/milo/releases/tag/v0.1.9) for the files below. Download from that release and compare each downloaded file’s SHA-256 with its entry in [SHA256SUMS](https://github.com/mcc0x/milo/releases/download/v0.1.9/SHA256SUMS) before running the installer. These checks detect a mismatched download; they don’t replace getting the files from the official release.

### macOS

On Apple Silicon with macOS 13 or later, install from the Homebrew tap:

```sh
brew install mcc0x/milo/milo
```

Or download [install.sh](https://github.com/mcc0x/milo/releases/download/v0.1.9/install.sh), check its hash and run it:

```sh
shasum -a 256 install.sh
sh install.sh
```

The shell installer downloads and checks the binary before placing it in `~/.local/bin` by default. Upgrade with `brew upgrade mcc0x/milo/milo` or rerun the shell installer from the newer release. If you installed the Rex integration, run `milo setup --repair` after upgrading. If you installed the macOS menu app, run `milo menubar install` again.

#### When Homebrew and Milo disagree

```sh
command -v milo
milo --version
brew list --versions mcc0x/milo/milo
```

`brew update` refreshes Homebrew and its formulae; it does not install or upgrade Milo. If `brew list --versions` shows no installed version, use `brew install mcc0x/milo/milo` to switch to Homebrew. If it lists an older version, use `brew upgrade mcc0x/milo/milo`. If Brew asks you to trust the formula, review the tap and trust just that formula with `brew trust --formula mcc0x/milo/milo`, then retry.

If Brew lists the expected version but the shell still finds a script- or source-installed copy first (often in `~/.local/bin`), either keep updating that copy with its installer or switch to Homebrew. Before switching, back up the old executable and any installer receipt. Once the Homebrew copy works, move the old executable out of PATH or reorder your PATH. Open a new shell and repeat the three checks above; `brew update` never replaces an unmanaged binary.

### Linux

On x86-64 Linux with glibc 2.35 or later, save [install-linux.sh](https://github.com/mcc0x/milo/releases/download/v0.1.9/install-linux.sh) and [milo-x86_64-unknown-linux-gnu](https://github.com/mcc0x/milo/releases/download/v0.1.9/milo-x86_64-unknown-linux-gnu) in the same directory. Check both hashes, then run the script there:

```sh
sha256sum install-linux.sh milo-x86_64-unknown-linux-gnu
sh install-linux.sh
```

The installer verifies the adjacent binary before placing it in `~/.local/bin` by default. To upgrade, repeat the steps with both files from the newer release. If `milo` isn’t on your PATH, run `~/.local/bin/milo`.

The release is qualified for standalone use on glibc x86-64, not musl, WSL or other architectures. It does not qualify Linux-hosted Rex.

### Windows

On native Windows x64, save [install-windows.ps1](https://github.com/mcc0x/milo/releases/download/v0.1.9/install-windows.ps1) and [milo-x86_64-pc-windows-msvc.exe](https://github.com/mcc0x/milo/releases/download/v0.1.9/milo-x86_64-pc-windows-msvc.exe) in the same folder. In PowerShell, open that folder, check both hashes, then run the verified script:

```powershell
Get-FileHash -Algorithm SHA256 -Path .\install-windows.ps1, .\milo-x86_64-pc-windows-msvc.exe
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install-windows.ps1
```

The policy option applies only to that PowerShell process. The executable is unsigned, so Windows may warn about an unknown publisher. The installer checks the adjacent executable, installs to `%LOCALAPPDATA%\Programs\Milo` by default and adds that directory to your user PATH. Open a new terminal and run `milo`. The Windows build runs standalone without Rex integration. Windows 11 with Windows Terminal was tested; WSL is not a native Windows install. To upgrade, close Milo and repeat the steps with both files from the newer release. To remove it, close Milo and run the installed `uninstall-milo.ps1` beside `milo.exe` the same way.

### Install paths and removal

The prebuilt installers don’t need Cargo, a compiler or administrator access. Set `MILO_BIN_DIR` before running an installer to choose a different directory (use a full path on Windows). For Homebrew use `brew uninstall mcc0x/milo/milo`. On macOS and Linux, run the shell installer with `--uninstall` and the same `MILO_BIN_DIR` if you set it. The scripts check their receipt before removing the binary and leave preferences in place. Older installs may not have a receipt; verify the binary before removing it manually.

### Build from source

Install Git and Rust 1.91 or later. On macOS and Linux, run:

```sh
git clone https://github.com/mcc0x/milo.git
cd milo
sh install.sh
```

On macOS and Linux, the source installer builds the checkout containing the script, even when launched from another directory. A standalone or piped copy clones Milo from GitHub into a private temporary directory instead. It honours `MILO_BIN_DIR` and defaults to `~/.local/bin`. On native Windows, use `cargo build --release --locked` in the checkout and run `target\release\milo.exe` instead of using the shell script.
