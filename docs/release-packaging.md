# Qualify the Apple Silicon release

Merge the version and packaging changes first. Keep the hosted installer, tap and public download links on the previous release until the new assets are live. The release binary cannot contain a pin to its own checksum.

## Keep qualification out of installation counts

Ordinary builds (including `cargo build --release` and `--all-features`), PRs and candidate packages have no production counts endpoint. An authorized distribution build must separately supply both the Rust compiler configuration `--cfg milo_distribution_counts` and a validated build-time HTTPS `MILO_COUNTS_ENDPOINT`. Neither has any effect as a runtime setting. Do not change the candidate workflow to test collection. The only positive reporting fixtures use a disposable binary compiled for a loopback receiver and throwaway Postgres. Never distribute that binary.

When testing **exact released bytes**, set `MILO_TELEMETRY=0` in the lab environment **before the first launch**, including terminal multiplexers, shells, child panes and helpers. On Unix export it before starting the session; on Windows set `$env:MILO_TELEMETRY = '0'` in the process starting Milo. Carry it explicitly through clean environment lists and service/VM launchers; a variable in one shell cannot affect a process another shell already started. It overrides a saved On preference, skips the notice and does not claim a day or queue a report. Do not identify VMs or exclude customers just because they use one.

## Retain the bytes the Mac job tested

The usual **Release tests** runs retain no installer. To qualify a release from `main`, dispatch that workflow with a one-time **public** recipient certificate. This repository is public, so the jobs upload only encrypted candidates. The workflow creates no tag or release.

In a private directory **outside the checkout**, on a Mac with OpenSSL and GitHub CLI:

```sh
umask 077
openssl req -x509 -newkey rsa:3072 -nodes -days 30 -subj /CN=milo-qualification \
  -keyout recipient.key -out recipient.pem
gh workflow run release-tests.yml --repo mcc0x/milo --ref main \
  -f recipient="$(base64 < recipient.pem | tr -d '\n')"
```

Never send the private key, a personal certificate or a password as an input. Check the dispatch's **head SHA** against the intended merged commit. Require a successful run with a non-skipped **Apple Silicon release tests** job on that SHA. Record the run ID and attempt, artifact ID, and the two SHA-256 values printed by **Seal the tested binary**: the plaintext CLI (`plaintext_sha256` below) and its embedded menu archive. A rerun or a different commit is a new candidate.

Using the recorded run ID and full source SHA:

```sh
gh run download "$run_id" --repo mcc0x/milo --name "milo-arm64-$source_sha"
openssl smime -decrypt -binary -inform DER -in milo.p7m -inkey recipient.key \
  -out milo-aarch64-apple-darwin
printf '%s  %s\n' "$plaintext_sha256" milo-aarch64-apple-darwin | shasum -a 256 -c -
chmod 755 milo-aarch64-apple-darwin
```

The Actions artifact expires after 14 days. Retain the verified bytes privately until the release decision. Its envelope and GitHub's artifact ZIP have different hashes from the executable; neither is a valid installer pin.

## Test, then promote without rebuilding

1. Inspect the archive embedded in the **downloaded** executable, including member ownership, metadata and paths. Check the outer CLI, extracted app and nested CLI signatures, architecture, minimum OS and version. Do not qualify a source build in their place.
2. Take `site/public/install.sh` from the recorded source SHA and change only its version and expected binary SHA-256 in staging. Generate `SHA256SUMS` for this installer and this executable. Use these three files for qualification and publication; the repository-root installer is for building from source.
3. Exercise fresh install, a checksum-verified upgrade from the preceding release with a saved preference, checksum refusal without replacement, and receipt-based removal. Test the installed menu, browser settings, standalone Reader and managed floating actions against a disposable Rex server. Use test profiles and stand-ins for outside services.
4. Obtain approval for the exact source SHA and asset digests. Create the new tag at that SHA only; never move an existing tag. Publish the verified executable, installer and sums together. Download the published files, verify their hashes and compare them to the staged bytes **before** updating the hosted installer, release links or tap. Updating the hosted installer on main can deploy the website immediately.

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

Compare the two entries in `linux/SHA256SUMS` to the job log. Keep those three extracted files privately; there is no installer to regenerate. Smoke-test the downloaded bytes on Ubuntu. Combine the verified Linux entries with the Mac entries in a **separate** release `SHA256SUMS` and check it against all four staged assets. Seek approval for the exact source SHA and these five files. Publish only to a **new** tag, compare fresh release downloads to staging, then update public links. No PR or ordinary push uploads a candidate.

This slice covers standalone text terminals and local, headless settings. Repeat the install and terminal smoke on a fresh Ubuntu amd64 guest, using a non-root user and a local candidate mirror. A plain SSH terminal can exercise standalone panes; it does not qualify a remote Rex desktop. This procedure does not qualify a graphical desktop or browser, tray, Linux-hosted Rex, live providers, arm64, 32-bit Linux, musl, WSL or Windows.
