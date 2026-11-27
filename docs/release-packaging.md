# Qualify release packages

Build from the intended versioned release commit. Keep the hosted installer, tap and public download links on the previous release until the new assets are live. The release binary cannot contain a pin to its own checksum.

## Keep qualification out of installation counts

Ordinary builds (including `cargo build --release` and `--all-features`), PRs and default candidates have no production counts endpoint. For an authorized reporting-enabled release, dispatch both package workflows on the same `release/prepare-*` ref with the recipient certificate and `-f distribution_counts=true`. The release build script supplies the compiler configuration and the pinned HTTPS URL; the dispatch accepts no arbitrary receiver URL. The setting has no effect at runtime. Record each run's source SHA, attempt and explicit counts mode with the retained hashes. Never mix packages from different modes.

Routine acceptance runs with counts off. To exercise reporting in the exact distribution, use a disposable lab with no route to production, route only the exact hostname to a controlled TLS receiver, and keep normal TLS certificate verification. The Apple Silicon and Windows jobs exercise their exact staged binary against private TLS before sealing. Native fresh-install and upgrade regressions still need independent acceptance. Never send synthetic reports to production. The separate loopback fixture builds a different executable and is never a release asset.

When testing **exact released bytes**, set `MILO_TELEMETRY=0` in the lab environment **before the first launch**, including terminal multiplexers, shells, child panes and helpers. On Unix export it before starting the session; on Windows set `$env:MILO_TELEMETRY = '0'` in the process starting Milo. Carry it explicitly through clean environment lists and service/VM launchers; a variable in one shell cannot affect a process another shell already started. It overrides a saved On preference, does not record the first launch and does not claim a day or queue a report. Do not identify VMs or exclude customers just because they use one.

## Retain the bytes the Mac job tested

The usual **Release tests** runs retain no installer. Dispatch that workflow on `main` or a `release/prepare-*` branch with a one-time **public** recipient certificate. Branch source in this repository is public; the jobs upload only encrypted candidates. The workflow creates no tag or release. A new source SHA, including a merge commit, needs its own qualification; see [the exact-commit tag check](../CONTRIBUTING.md#tagging-a-release).

In a private directory **outside the checkout**, on a Mac with OpenSSL and GitHub CLI:

```sh
umask 077
openssl req -x509 -newkey rsa:3072 -nodes -days 30 -subj /CN=milo-qualification \
  -keyout recipient.key -out recipient.pem
gh workflow run release-tests.yml --repo mcc0x/milo --ref "${ref:?set the release source branch}" \
  -f recipient="$(base64 < recipient.pem | tr -d '\n')"
```

Never send the private key, a personal certificate or a password as an input. Check the dispatch's **head SHA** against the intended commit. Require a successful run with a non-skipped **Apple Silicon release tests** job on that SHA. Record the run ID and attempt, artifact ID, and the plaintext hashes printed by **Seal the tested Mac release files**: the tar (`mac_tar_sha256` below), the two package files, and the embedded menu archive. A rerun, a different commit or a changed counts mode is a new candidate.

Using the recorded run ID and full source SHA:

```sh
gh run download "$run_id" --repo mcc0x/milo --name "milo-arm64-$source_sha" --dir mac
(
  cd mac || exit 1
  set -e
  openssl smime -decrypt -binary -inform DER -in milo-macos.tar.p7m \
    -inkey ../recipient.key -out milo-macos.tar
  printf '%s  milo-macos.tar\n' "$mac_tar_sha256" | shasum -a 256 -c -
  tar -xf milo-macos.tar
  shasum -a 256 -c SHA256SUMS
)
```

The Actions artifact expires after 14 days. Compare the two manifest entries to the job log and retain the verified files privately until the release decision. The tar, its envelope, and GitHub's artifact ZIP have different hashes from the executable; none is a valid installer pin.

## Test, then promote without rebuilding

1. Inspect the archive embedded in the **downloaded** executable, including member ownership, metadata and paths. Check the outer CLI, extracted app and nested CLI signatures, architecture, minimum OS and version. Do not qualify a source build in their place.
2. Check that the retained `install.sh` pins the tested version and executable digest. Staging changes only these two values from `site/public/install.sh` at the recorded source SHA. Exercise the retained installer and executable together; do not recreate either for publication. The repository-root installer is for building from source.
3. Exercise fresh install, a checksum-verified upgrade from the preceding release with a saved preference, checksum refusal without replacement, and receipt-based removal. Test the installed menu, browser settings, standalone Reader and managed floating actions against a disposable Rex server. Use test profiles and stand-ins for outside services.
4. Keep the verified executable and installer together for the combined release manifest below. Updating the hosted installer on main can deploy the website immediately, so wait until after publication and verification.

## Qualify standalone Linux separately

**Candidate only.** v0.1.8 has no Linux download. This procedure does not change the source installer, the hosted Mac installer, the tap or any release. Ubuntu 22.04 LTS amd64 is the oldest test target: it provides glibc 2.35, a maintained Jammy userspace and a hosted runner for the real terminal tests. The baseline is the system we run, not the highest glibc symbol found in an ELF. A newer glibc permits installation but does not qualify another distribution.

On a non-root x86-64 Linux builder with Docker and rustup, start in a clean checkout of the **committed revision**. No local uncommitted changes go into the binary.

```sh
umask 077
assets=$(mktemp -d)
sh .github/ci/qualify-linux.sh "$assets"
sh tests/linux-install.sh
sh tests/linux-candidate.sh "$assets"  # needs tmux, Git and Python 3 for the tests
```

The first command after creating the directory builds twice in digest-pinned Jammy builders with Rust 1.91.0, a locked dependency and different source and target directories. It requires identical ELF bytes, checks the loader, library dependencies, symbol versions and build paths, and binds the offline installer to the executable's SHA-256. A separate read-only Jammy runtime, with no network, compiler or repository, verifies the sums and exercises the installed executable. The last command drives the installed file, not a Cargo target: Home → Outline → Reader save and conflict, History, terminal restoration, headless settings save/reopen and missing usage providers. Use that same directory unchanged on each proposed target; record the source SHA, plaintext checksums and runtime results.

**User steps for a future authorized release:** use an empty directory. Save `SHA256SUMS`, `install-linux.sh` and `milo-x86_64-unknown-linux-gnu` from the **same trusted release**. With GitHub CLI, set `tag` to the published Linux release tag and download just those files:

```sh
gh release download "${tag:?set the published Linux tag}" --repo mcc0x/milo \
  --pattern SHA256SUMS --pattern install-linux.sh --pattern milo-x86_64-unknown-linux-gnu
test -f install-linux.sh && test -f milo-x86_64-unknown-linux-gnu &&
  grep -Eq '^[0-9a-f]{64}  install-linux\.sh$' SHA256SUMS &&
  grep -Eq '^[0-9a-f]{64}  milo-x86_64-unknown-linux-gnu$' SHA256SUMS &&
  sha256sum --check --ignore-missing SHA256SUMS
```

Both the installer and the Linux binary must report `OK` before you run the script. The combined manifest also lists Mac files you didn't download. With Git available for History, run:

```sh
sh install-linux.sh                # verifies the adjacent ELF before replacing anything
"$HOME/.local/bin/milo"            # no PATH change, checkout, sudo, Cargo or Rex
sh install-linux.sh --uninstall    # optional: removes only a matching binary and receipt
```

Repeat the install from a new release directory to upgrade. Existing preferences and adjacent files stay in place. The installer refuses a changed binary, an unmanaged installation, or an old or unknown libc. To choose another directory, set `MILO_BIN_DIR` for both install and removal. The checksum detects damaged or mismatched bytes; it does not authenticate a script from an untrusted source.

**Retain and promote:** use the same dispatch and private key as for Mac. Require both the non-skipped Apple Silicon job and the Linux candidate job on the intended version commit. Record the run ID, attempt, artifact IDs, and the Linux job's plaintext hashes for the tar, ELF and installer. Before the 14-day expiry, download into a fresh private directory. Set `linux_tar_sha256` from the job log, not from the downloaded archive:

```sh
gh run download "$run_id" --repo mcc0x/milo --name "milo-linux-x86_64-$source_sha" --dir linux
(
  cd linux || exit 1
  set -e
  openssl smime -decrypt -binary -inform DER -in milo-linux.tar.p7m \
    -inkey ../recipient.key -out milo-linux.tar
  printf '%s  milo-linux.tar\n' "$linux_tar_sha256" | shasum -a 256 -c -
  tar -xf milo-linux.tar
  shasum -a 256 -c SHA256SUMS
)
```

Compare the two entries in `linux/SHA256SUMS` to the job log. Keep those three extracted files privately; there is no installer to regenerate. Smoke-test the downloaded bytes on Ubuntu. No PR or ordinary push uploads a candidate.

This slice covers standalone text terminals and local, headless settings. Repeat the install and terminal smoke on a fresh Ubuntu amd64 guest, using a non-root user and a local candidate mirror. A plain SSH terminal can exercise standalone panes; it does not qualify a remote Rex desktop. This procedure does not qualify a graphical desktop or browser, tray, Linux-hosted Rex, live providers, arm64, 32-bit Linux, musl, WSL or Windows.

## Include Windows when the release source has the native installer

Dispatch the **Windows** workflow on the same branch with the same recipient certificate. Require the non-skipped **Native Windows x86-64 tests** job on the same full source SHA; the separate counts workflow produces a test fixture, not a release package.

```sh
gh workflow run windows.yml --repo mcc0x/milo --ref "${ref:?set the release source branch}" \
  -f recipient="$(base64 < recipient.pem | tr -d '\n')"
```

Record the Windows run ID, attempt, artifact ID and the plaintext ZIP, executable and installer hashes printed by **Seal the tested installer package for its recipient**. Download before the one-day retention limit. Use the plaintext ZIP hash from the job log as `windows_zip_sha256`.

```sh
gh run download "$windows_run_id" --repo mcc0x/milo \
  --name "milo-windows-x86_64-release-$source_sha" --dir windows
(
  cd windows || exit 1
  set -e
  openssl smime -decrypt -binary -inform DER -in milo-windows.zip.p7m \
    -inkey ../recipient.key -out milo-windows.zip
  printf '%s  milo-windows.zip\n' "$windows_zip_sha256" | shasum -a 256 -c -
  unzip milo-windows.zip
  shasum -a 256 -c SHA256SUMS
)
```

Compare the two manifest entries to the log. Copy those same package files to a clean Windows x64 guest. As a standard user, exercise `install-windows.ps1`, the installed executable, a new shell's user PATH, and the installed uninstaller. Check that reinstalling adds no duplicate PATH entry and removing the package leaves settings intact. There was no public v0.1.8 Windows package; use a known prior private package for an upgrade check and label it as such.

## Assemble one release manifest

Copy only the verified executables and their installers into a fresh staging directory. Concatenate the per-platform manifests for included platforms into a separate release `SHA256SUMS`; do not put a manifest's own digest in it. From the staging directory, require `shasum -a 256 -c SHA256SUMS` to verify every file, with one line per asset. Record that combined manifest's own digest in the private sign-off. Seek approval for the exact source SHA and the exact staged file set. Create the new tag at that SHA only; never move an existing tag. Compare fresh release downloads to the verified files before updating public links.
