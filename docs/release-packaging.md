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
