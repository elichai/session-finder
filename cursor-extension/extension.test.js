const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const { runInNewContext } = require('node:vm');

const id = '9743f739-3305-4167-84cc-af076a41c290';
const cwd = '/tmp/session project';
const query = `id=${id}&cwd=%2Ftmp%2Fsession%20project`;

function fixture({ glass = true, result = true, failure, folder = cwd, pending } = {}) {
  const calls = [], errors = [], logs = [];
  let handler;
  const vscode = {
    Uri: { file: fsPath => ({ scheme: 'file', fsPath }) },
    workspace: { workspaceFolders: folder ? [{ uri: { scheme: 'file', fsPath: folder } }] : [] },
    window: {
      createOutputChannel: () => ({ info: text => logs.push(text), error: text => logs.push(text) }),
      registerUriHandler: value => { handler = value; return {}; },
      showErrorMessage: text => { errors.push(text); },
    },
    commands: {
      getCommands: async () => glass ? ['glass.openAgentById'] : ['_workbench.open'],
      executeCommand: async (...args) => {
        calls.push(args);
        if (failure) throw failure;
        return result;
      },
    },
  };
  const module = { exports: {} };
  runInNewContext(readFileSync(require.resolve('./extension/extension.js'), 'utf8'), {
    module, exports: module.exports, URLSearchParams,
    require: name => name === 'node:path' ? require(name) : (assert.equal(name, 'vscode'), vscode),
  });
  const ready = module.exports.activate({ subscriptions: [], globalState: {
    get: () => pending,
    update: async (_key, value) => { pending = value; },
  } });
  return {
    calls, errors, logs, ready, pending: () => pending,
    open: async (changes = {}) => {
      await ready;
      assert.ok(handler, 'extension must register its URI handler');
      await handler.handleUri({ scheme: 'cursor', authority: 'session-finder.cursor-session-opener',
        path: '/open', query, fragment: '', ...changes });
    },
  };
}

test('opens the exact Glass session and logs verified success', async () => {
  const app = fixture();
  await app.open();
  assert.deepEqual(app.calls.map(call => Array.from(call)), [['glass.openAgentById', id]]);
  assert.equal(app.errors.length, 0);
  assert.ok(app.logs.some(line => line.includes('Opened') && line.includes(id)));
});

test('rejects missing sessions without falling back or reporting success', async () => {
  for (const result of [false, null, 0]) {
    const app = fixture({ result });
    await app.open();
    assert.equal(app.calls.length, 1);
    assert.equal(app.errors.length, 1);
    assert.ok(!app.logs.some(line => line.startsWith('Opened')));
  }
});

test('classic fallback sends the internal local session URI', async () => {
  const app = fixture({ glass: false });
  await app.open();
  assert.deepEqual(app.calls.map(call => Array.from(call)), [['_workbench.open', `cursor.agent://local/${id}`]]);
  assert.equal(app.errors.length, 0);
  assert.ok(!app.logs.some(line => line.startsWith('Opened')));
});

test('rejects malformed and unsupported routes before invoking commands', async () => {
  for (const changes of [
    { query: 'id=draft' }, { query: 'id=' }, { query: '' },
    { query: `id=${id}&id=${id}` }, { query: `id=${id}&prompt=hello` },
    { query: `id=${id}` }, { query: `id=${id}&cwd=relative` },
    { query: `id=${id}&cwd=%2Ftmp%00bad` }, { query: `${query}&cwd=%2Fother` },
    { scheme: 'https' }, { authority: 'other.extension' }, { path: '/send' },
    { fragment: 'message' }, { query: 'id=9743f739-3305-4167-84cc-af076a41c29' },
  ]) {
    const app = fixture();
    await app.open(changes);
    assert.equal(app.calls.length, 0);
    assert.equal(app.errors.length, 1);
  }
});

test('routes to the session directory before opening a chat', async () => {
  const app = fixture({ folder: '/tmp/other' });
  await app.open();
  assert.equal(app.calls.length, 1);
  assert.equal(app.calls[0][0], 'vscode.openFolder');
  assert.equal(app.calls[0][1].fsPath, cwd);
  assert.equal(app.calls[0][2].forceReuseWindow, true);
  assert.equal(app.pending().id, id);
  assert.equal(app.pending().cwd, cwd);
  assert.ok(Number.isFinite(app.pending().requestedAt));
});

test('startup resumes pending chat once in its matching directory', async () => {
  const app = fixture({ pending: { id, cwd, requestedAt: Date.now() } });
  await app.ready;
  assert.deepEqual(app.calls.map(call => Array.from(call)), [['glass.openAgentById', id]]);
  assert.equal(app.pending(), undefined);
});

test('startup in another window does not steal the pending chat', async () => {
  const app = fixture({ folder: '/tmp/other', pending: { id, cwd, requestedAt: Date.now() } });
  await app.ready;
  assert.equal(app.calls.length, 0);
  assert.equal(app.pending().id, id);
});

test('expired pending chat clears on startup without opening anything', async () => {
  const app = fixture({ pending: { id, cwd, requestedAt: Date.now() - 120001 } });
  await app.ready;
  assert.equal(app.calls.length, 0);
  assert.equal(app.pending(), undefined);
});

test('failed folder routing clears pending chat and reports error', async () => {
  const emptyApp = fixture({ folder: null, failure: new Error('cannot open folder') });
  await emptyApp.open();
  assert.equal(emptyApp.calls[0][0], 'vscode.openFolder');
  assert.equal(emptyApp.pending(), undefined);
  assert.match(emptyApp.errors[0], /cannot open folder/);
});

test('reports command errors without invoking a fallback', async () => {
  const app = fixture({ failure: new Error('unavailable') });
  await app.open();
  assert.equal(app.calls.length, 1);
  assert.match(app.errors[0], /unavailable/);
  assert.ok(app.logs.some(line => line.includes('unavailable')));
});
