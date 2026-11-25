# Qualify the Apple Silicon release

Merge the version and packaging changes first. Keep the hosted installer, tap and public download links on the previous release until the new assets are live. The release binary cannot contain a pin to its own checksum.

## Retain the bytes the Mac job tested

The usual **Release tests** runs retain no installer. To qualify a release from `main`, dispatch that workflow with a one-time **public** recipient certificate. This repository is public, so the job uploads only an encrypted copy of its tested CLI, never a plaintext candidate. It creates no tag or release.

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

**User steps for a future authorized release:** obtain `install-linux.sh` and `milo-x86_64-unknown-linux-gnu` from the **same trusted release** and put them together. With Git available for History, open a terminal and run:

```sh
sh install-linux.sh                # verifies the adjacent ELF before replacing anything
"$HOME/.local/bin/milo"            # no PATH change, checkout, sudo, Cargo or Rex
sh install-linux.sh --uninstall    # optional: removes only a matching binary and receipt
```

Repeat the install from a new release directory to upgrade. Existing preferences and adjacent files stay in place. The installer refuses a changed binary, an unmanaged installation, or an old or unknown libc. To choose another directory, set `MILO_BIN_DIR` for both install and removal. The checksum detects damaged or mismatched bytes; it does not authenticate a script from an untrusted source.

For promotion, the existing **Release tests** dispatch on the approved `main` SHA can seal both targets to the same one-time public certificate. Require the non-skipped Apple Silicon job **and** the Linux candidate job, record the run/attempt and plaintext hashes, and decrypt the separate `milo-linux-x86_64-$source_sha` artifact (`milo-linux.p7m`) as above. Recreate the Linux installer with `stage-linux.sh` using the recorded one-entry ELF checksum file named `SHA256SUMS`; compare **both** hashes against the job log. Combine the Mac and Linux entries into a single release manifest. Qualify those exact bytes, then seek approval for a **new** tag and release; never retrofit an old tag. No PR or ordinary push uploads a candidate.

This slice covers standalone text terminals and local, headless settings. It does not qualify a graphical desktop or browser, tray, SSH, Linux-hosted Rex, live providers, arm64, 32-bit Linux, musl, WSL or Windows.
