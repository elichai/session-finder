const assert = require('node:assert/strict');
const { test } = require('node:test');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { verifySession, projectKey } = require('./extension/claude');

const id = '9743f739-3305-4167-84cc-af076a41c290';

async function fixture(t, settings = {}) {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), 'session-finder-claude-')));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const cwd = path.join(root, 'workspace');
  const projects = path.join(root, 'store', 'projects');
  const binary = path.join(root, 'resources', 'native-binary', 'claude');
  await fs.mkdir(cwd, { recursive: true });
  await fs.mkdir(path.dirname(binary), { recursive: true });
  await fs.writeFile(binary, `#!${process.execPath}\n` +
    `if (JSON.stringify(process.argv.slice(2)) !== '["auth","status","--json"]') process.exit(2);\n` +
    `if (process.env.EXPECT_CWD !== process.cwd()) process.exit(3);\n` +
    `console.log(process.env.AUTH_FIXTURE || JSON.stringify({projectsDirectory: process.env.PROJECTS, configDirectory: require('node:path').dirname(process.env.PROJECTS)}));\n` +
    `process.exitCode = Number(process.env.AUTH_EXIT || 0);\n`, { mode: 0o700 });
  const vscode = { workspace: { getConfiguration: name => {
    assert.equal(name, 'claudeCode');
    return { get: key => ({ ...settings, environmentVariables: [
      { name: 'PROJECTS', value: projects }, { name: 'EXPECT_CWD', value: cwd },
      ...(settings.environmentVariables || []),
    ] })[key] };
  } } };
  const target = { id, cwd };
  const file = path.join(projects, cwd.replace(/[^a-zA-Z0-9]/g, '-'), `${id}.jsonl`);
  const row = { type: 'user', sessionId: id, cwd, parentUuid: null, message: { role: 'user', content: 'Fixture prompt' } };
  const write = async rows => {
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, rows.map(value => JSON.stringify(value)).join('\n') + '\n');
  };
  await write([row]);
  return { root, cwd, file, row, write, vscode, extension: { extensionPath: root }, target };
}

test('verifies an ordinary persisted session using native resolved store and workspace', async t => {
  const app = await fixture(t);
  await assert.doesNotReject(verifySession(app.vscode, app.extension, app.target));
});

test('uses valid store metadata when native auth reports signed out with exit one', async t => {
  const app = await fixture(t, { environmentVariables: [{ name: 'AUTH_EXIT', value: '1' }] });
  await assert.doesNotReject(verifySession(app.vscode, app.extension, app.target));
});

test('accepts extension text blocks with context and multiline code', async t => {
  const app = await fixture(t);
  await app.write([
    { ...app.row, isMeta: true, message: { role: 'user', content: [{ type: 'text', text: '<ide_opened_file>Context</ide_opened_file>' }] } },
    { ...app.row, message: { role: 'user', content: [
      { type: 'text', text: '<ide_selection>Context</ide_selection>' },
      { type: 'text', text: 'Explain this code:\nif a < b { return a; }' },
    ] } },
  ]);
  await assert.doesNotReject(verifySession(app.vscode, app.extension, app.target));
});

test('rejects missing, nonregular, and symlink transcripts', async t => {
  const app = await fixture(t);
  const backup = `${app.file}.saved`;
  await fs.rename(app.file, backup);
  await assert.rejects(verifySession(app.vscode, app.extension, app.target));
  await fs.mkdir(app.file);
  await assert.rejects(verifySession(app.vscode, app.extension, app.target));
  await fs.rmdir(app.file);
  await fs.symlink(backup, app.file);
  await assert.rejects(verifySession(app.vscode, app.extension, app.target));
});

test('rejects native-list exclusions, mismatched identity, and uncertain metadata', async t => {
  const app = await fixture(t);
  for (const patch of [
    { isSidechain: true }, { entrypoint: 'sdk-cli' }, { entrypoint: 'sdk-ts' },
    { entrypoint: 'sdk-py' }, { sessionKind: 'daemon' }, { sessionKind: 'daemon-worker' },
    { sessionId: '0743f739-3305-4167-84cc-af076a41c290' }, { cwd: app.root },
    { isMeta: true }, { isCompactSummary: true },
    { message: { content: '<local-command-stdout>ignored</local-command-stdout>' } },
    { message: { content: [{ type: 'tool_result', content: 'ignored' }] } },
  ]) {
    await app.write([{ ...app.row, ...patch }]);
    await assert.rejects(verifySession(app.vscode, app.extension, app.target));
  }
  for (const type of ['continued-in', 'relocated']) {
    await app.write([app.row, { type, continuedInSessionId: id, relocatedCwd: app.cwd }]);
    await assert.rejects(verifySession(app.vscode, app.extension, app.target));
  }
});

test('uses bounded head and tail checks and rejects a hidden tail exclusion', async t => {
  const app = await fixture(t);
  await app.write([app.row, { type: 'assistant', message: { content: 'x'.repeat(200000) } }]);
  await assert.doesNotReject(verifySession(app.vscode, app.extension, app.target));
  await fs.appendFile(app.file, JSON.stringify({ type: 'continued-in', continuedInSessionId: id }) + '\n');
  await assert.rejects(verifySession(app.vscode, app.extension, app.target));
});

test('requires user evidence in the native head sample and handles Unicode and newlines', async t => {
  const app = await fixture(t);
  const large = { type: 'assistant', message: { content: 'x'.repeat(200000) } };
  await app.write([large, app.row]);
  await assert.rejects(verifySession(app.vscode, app.extension, app.target));
  await app.write([{ ...app.row, message: { content: 'µ'.repeat(100) + '\nSecond line' } }, large]);
  await assert.doesNotReject(verifySession(app.vscode, app.extension, app.target));
});

test('rejects native exclusions visible in an incomplete tail row', async t => {
  const app = await fixture(t);
  await app.write([app.row, { type: 'assistant', message: { content: 'x'.repeat(200000) }, entrypoint: 'sdk-ts' }]);
  await assert.rejects(verifySession(app.vscode, app.extension, app.target));
});

test('rejects wrappers and malformed auth output without exposing its contents', async t => {
  const app = await fixture(t, { claudeProcessWrapper: '/private/wrapper' });
  await assert.rejects(verifySession(app.vscode, app.extension, app.target));
  const malformed = await fixture(t, { environmentVariables: [
    { name: 'AUTH_FIXTURE', value: '{"email":"secret@example.com"}' },
  ] });
  await assert.rejects(verifySession(malformed.vscode, malformed.extension, malformed.target), error => {
    assert.ok(!error.message.includes('secret@example.com'));
    assert.match(error.message, /did not report/);
    return true;
  });
});

test('matches native encoding for punctuation, long names, and explicit project override', () => {
  assert.equal(projectKey('/a/b_c'), '-a-b-c');
  assert.equal(projectKey('a'.repeat(201)), `${'a'.repeat(200)}-rkvsv5`);
  assert.equal(projectKey('/x', { CLAUDE_CONFIG_DIR: '/store', CLAUDE_CODE_PROJECT_DIR_NAME: 'custom' }), 'custom');
  assert.equal(projectKey('/x', { CLAUDE_CODE_PROJECT_DIR_NAME: 'custom' }), '-x');
  assert.throws(() => projectKey('/x', { CLAUDE_CONFIG_DIR: '/store', CLAUDE_CODE_PROJECT_DIR_NAME: '../escape' }));
});
