# Recovery checks against `9576854`

These checks used disposable homes and state on arm64 macOS. The initial paste, canceled decoder, server-stop and alert failures were reproduced against the corresponding code on main (`9576854`). Review also reproduced encoding drift on re-keep and editing, a completed probe leaking its input descriptor to a helper, and a tunnel-only close. No provider, personal Rex session or installed Milo settings were used.

| Failure injected | Observed before correction | Recovery invariant |
| --- | --- | --- |
| Paste a leading U+FEFF in an originally unmarked file; end the reader externally, reopen and restore. | Restoring `\u{feff}abc\n` yielded `abc\n`. The only edit disappeared. | A pasted mark is editable text. Preserve it through recovery, undo/redo and explicit save. |
| Restore while the working file's BOM differs from its original encoding. Type at the start, close without saving, revert the disk version, then recover again. | Re-keeping could add a BOM to an unmarked draft; a marked draft could end up as `\u{feff}later \u{feff}kept abc\n`, with its structural mark moved into the body. | Keep structural marks outside editable text and preserve the source encoding on re-keep. Explicit overwrite uses the current working file's encoding. |
| Paste and save a mark into an unmarked file, then retain, move or delete it and interrupt the editor. | Reopening classified the saved disk prefix as structural as well as editable. Saving added an extra leading BOM in all three cases. | Restoration over the same base resumes the source editor's encoding, including across further saves and undo/redo. Comparing shows the bytes that saving would write. |
| Close an MP4 job while a finite decoder helper holds stdout open. | The direct child died, but closing the preview was still waiting at the fixture's two-second release gate. | Stop the decoder's own process group before reaping the child and joining the output reader. Do not signal a group after reaping its leader. |
| Let a successful probe exit while its helper retains the snapshot descriptor but not stdout. Release the helper after the next decoder rewinds. | The helper consumed the first byte of the next decoder's input; the decoder rejected the header and showed no frame. | On normal completion, observe exit without reaping, stop the owned group, then reap and use the leader's original exit status. |
| Stop the remote Rex server after its forwarded editor served state, or close just its tunnel. | The local port closed and local settings stayed unchanged, but `milo web --host` exited zero. In the server-stop case it said the editor ended; in the tunnel-only case the editor was still running. | A failed watcher or forward, or a forward that closes without editor completion, is not a successful session. Local teardown still runs. Done and Ctrl-C remain clean exits. |
| Reject re-saving an identical active destination while a synthetic permission alert is in flight. Release a loopback HTTP 429 after the save. | The command reported a write error and mirroring still reported `on`, but the next delivery was the later `finished` event. The pending permission alert was gone. | A rejected identical re-save must leave the current mirror and its queue alone. After the rate limit, the pending alert arrives before the later event. Replacing a credential still disarms its old target first. |

A separate 160×50 PTY smoke test opened `milo view notes.txt --edit` on `abc\n`, sent a bracketed paste of U+FEFF followed by `X`, waited for the edit to appear, then sent SIGTERM to that reader. The working file stayed unchanged. Reopen → restore → Ctrl-S saved `58 61 62 63 0a` on main and `ef bb bf 58 61 62 63 0a` after the fix.

## Reproduce on the PR branch

Run from the checkout. The host lab needs an installed Rex CLI (`MILO_TEST_REX` can name it); it starts two separate servers on private Unix sockets. The alert test needs a **debug** build and uses a fake Rex stream, loopback HTTP and, on macOS, a disposable explicitly named keychain. Release builds skip that integration test.

```sh
cargo test --locked --bin milo recovering_a_kept_draft_
cargo test --locked --bin milo rekeeping_an_unconfirmed_draft_preserves_its_source_encoding
cargo test --locked --bin milo competing_readers_keep_both_edits_to_the_same_restored_draft
cargo test --locked --bin milo dropping_a_video_job_does_not_wait_for_a_decoder_helper
cargo test --locked --bin milo a_finished_probe_cannot_consume_the_next_decoders_snapshot
cargo test --locked --test rex_hosts -- --ignored
cargo test --locked --test alerts a_change_that_cannot_be_saved_leaves_mirroring_running -- --exact
```

Two existing scope checks also passed without production changes: `stale_layout_save_and_inherit_cannot_replace_a_newer_profile` and `preferences_share_scope_and_guard_stale_saves_and_inheritance` (`cargo test --locked --bin milo FILTER`). They stage competing saves and attempts to inherit from stale snapshots, then check the stored worktree value.

## Boundaries

- Two readers restored the same predecessor before either closed. Both successors remained recoverable and a second save could not clobber the first without confirmation. This is a deterministic interleaving, not simultaneous filesystem calls, abrupt power loss or a crash during `fsync`.
- New kept copies with a leading U+FEFF tag its role (`bom` or `pasted`) in the filename; the payload stays raw UTF-8. Older single-copy, numbered and base-recording names remain readable. If an untagged draft's base cannot establish the role of its prefix, recovery treats it as structural, as before. No migration is required.
- Decoder failures were injected with ordinary helpers inheriting the private group, not a daemon leaving it. The successful playback/seek/reopen path also passed with a generated clip and real FFmpeg/FFprobe 9.0.2 (`cargo test --locked --bin milo non_grid_eof_bookmark_restores_the_last_sample_across_tabs -- --ignored`).
- The host check is a live loopback Rex disconnect, not an SSH partition. An unreachable remote host cannot confirm remote cleanup. The test checks local exit status, port closure and unchanged local settings, not destruction of an unreachable session.
- The alert test checks a known retryable rejection, not exactly-once delivery after an ambiguous network failure.
