import assert from 'node:assert/strict';
import { test } from 'node:test';
import { edit, frameAt, newCommit } from '../src/scene.ts';

test('the file and commit panes only get new data when they change', () => {
  const initial = frameAt('compose', 'claude');
  const editing = frameAt('editing', 'claude');
  const edited = frameAt('edit', 'claude');
  const reading = frameAt('read', 'claude');
  const committed = frameAt('commit', 'claude');
  const rest = frameAt('rest', 'claude');

  assert.equal(editing.files, initial.files);
  assert.equal(reading.files, edited.files);
  assert.equal(edited.files.find(file => file.path === edit.path)?.pending, 'commit');
  assert.equal(edited.toCommit, initial.toCommit + 1);
  assert.equal(edited.ahead, initial.ahead);
  assert.equal(committed.files.find(file => file.path === edit.path)?.pending, 'push');
  assert.equal(committed.toCommit, initial.toCommit);
  assert.equal(committed.toPush, initial.toPush + 1);
  assert.equal(committed.ahead[0], newCommit);
  assert.equal(rest.files, committed.files);
  assert.equal(rest.ahead, committed.ahead);
});

test('Usage keeps provider-specific reports through the resize and new tab', () => {
  const initial = frameAt('rest', 'claude').usage;
  const claude = frameAt('usage', 'claude').usage;
  const codex = frameAt('usage', 'codex').usage;

  assert.deepEqual(claude, { claude: initial.claude - 2, codex: initial.codex });
  assert.deepEqual(codex, { claude: initial.claude, codex: initial.codex - 2 });
  assert.equal(frameAt('usage', 'omp').usage, codex);
  assert.equal(frameAt('resize', 'claude').usage, claude);
  assert.equal(frameAt('new-tab', 'claude').usage, claude);
  assert.equal(frameAt('compose', 'codex').usage, initial);
});
