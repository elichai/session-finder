const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { runInNewContext } = require('node:vm');

const id = '9743f739-3305-4167-84cc-af076a41c290';
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'session-finder-routing-'));
const cwd = path.join(root, 'session project');
const other = path.join(root, 'other');
fs.mkdirSync(cwd);
fs.mkdirSync(other);
const canonical = fs.realpathSync(cwd);
const alias = path.join(root, 'alias');
fs.symlinkSync(cwd, alias);
const regularFile = path.join(root, 'file');
fs.writeFileSync(regularFile, 'fixture');
const query = (provider = 'claude', directory = cwd) =>
  new URLSearchParams({ provider, id, cwd: directory }).toString();

function fixture({ folders = [cwd], pending, storage = new Map(), missing = false, verifyError, commandError,
  activateError, activateAction, verifyAction, folderError, scheme = 'vscode' } = {}) {
  const calls = [], errors = [], logs = [], verifications = [], activations = [];
  if (pending !== undefined) storage.set('pendingSession:fixture', pending);
  let handler;
  const uri = value => {
    const parsed = new URL(value);
    return { scheme: parsed.protocol.slice(0, -1), authority: parsed.hostname,
      path: parsed.pathname, query: parsed.search.slice(1), fragment: parsed.hash.slice(1),
      toString: () => value };
  };
  const vscode = {
    env: { uriScheme: scheme },
    Uri: { file: fsPath => ({ scheme: 'file', fsPath }), parse: uri },
    workspace: { workspaceFolders: folders.map(folder => ({ uri: typeof folder === 'string'
      ? { scheme: 'file', fsPath: folder } : folder })) },
    extensions: { getExtension: name => missing ? undefined : {
      id: name, isActive: false,
      activate: async () => {
        activations.push(name);
        if (activateError) throw activateError;
        await activateAction?.(vscode.workspace);
        return {};
      },
    } },
    window: {
      createOutputChannel: () => ({ info: value => logs.push(value), error: value => logs.push(value),
        warn: value => logs.push(value), appendLine: value => logs.push(value), dispose() {} }),
      registerUriHandler: value => { handler = value; return { dispose() {} }; },
      showErrorMessage: value => { errors.push(value); },
    },
    commands: { executeCommand: async (...args) => {
      calls.push(args);
      if (args[0] === 'vscode.openFolder' && folderError) throw folderError;
      if (args[0] !== 'vscode.openFolder' && commandError) throw commandError;
    } },
  };
  const module = { exports: {} };
  runInNewContext(fs.readFileSync(path.join(__dirname, 'extension', 'extension.js'), 'utf8'), {
    module, exports: module.exports, URLSearchParams, URL, console,
    require: name => name === 'vscode' ? vscode : name === './claude' || name === './claude.js'
      ? { verifySession: async (...args) => {
        verifications.push(args);
        if (verifyError) throw verifyError;
        await verifyAction?.(vscode.workspace);
      } } : require(name),
  });
  const ready = module.exports.activate({ subscriptions: [], globalState: {
    keys: () => [...storage.keys()],
    get: key => storage.get(key),
    update: async (key, value) => { if (value === undefined) storage.delete(key); else storage.set(key, value); },
  } });
  return { calls, errors, logs, activations, verifications, ready, storage,
    pending: () => storage.values().next().value,
    open: async (changes = {}) => {
      await ready;
      assert.ok(handler, 'helper must register its URI handler');
      await handler.handleUri({ scheme, authority: 'session-finder.vscode-session-opener',
        path: '/open', query: query(), fragment: '', ...changes });
    } };
}

const commandNames = app => app.calls.map(call => call[0]);

test('opens verified Claude editor in its exact single workspace', async () => {
  const app = fixture();
  await app.open();
  assert.deepEqual(app.activations, ['anthropic.claude-code']);
  assert.equal(app.verifications.length, 1);
  assert.equal(app.verifications[0][2].cwd, canonical);
  assert.equal(app.verifications[0][2].id, id);
  assert.deepEqual(Array.from(app.calls[0]), ['claude-vscode.primaryEditor.open', id, undefined]);
  assert.equal(app.errors.length, 0);
});

test('opens Codex existing conversation in a focused permanent custom editor', async () => {
  const app = fixture();
  await app.open({ query: query('codex') });
  assert.deepEqual(app.activations, ['openai.chatgpt']);
  assert.equal(app.verifications.length, 0);
  assert.equal(app.calls.length, 1);
  const [name, resource, type, options] = app.calls[0];
  assert.equal(name, 'vscode.openWith');
  assert.equal(resource.toString(), `openai-codex://route/local/${id}`);
  assert.equal(type, 'chatgpt.conversationEditor');
  assert.equal(options.preview, false);
  assert.equal(options.preserveFocus, false);
  assert.equal(app.errors.length, 0);
});

test('supports the running VS Code Insiders URI scheme', async () => {
  const app = fixture({ scheme: 'vscode-insiders' });
  await app.open();
  assert.deepEqual(commandNames(app), ['claude-vscode.primaryEditor.open']);
});

test('canonicalizes target symlinks before matching the workspace', async () => {
  const app = fixture({ folders: [alias] });
  await app.open({ query: query('claude', alias) });
  assert.deepEqual(commandNames(app), ['claude-vscode.primaryEditor.open']);
  assert.equal(app.verifications[0][2].cwd, canonical);
});

test('wrong, empty, remote, and multiple folder workspaces route to a new single-folder window', async () => {
  for (const folders of [[other], [], [other, cwd], [cwd, other],
    [{ scheme: 'vscode-remote', fsPath: cwd }]]) {
    const app = fixture({ folders });
    await app.open();
    assert.deepEqual(commandNames(app), ['vscode.openFolder']);
    assert.equal(app.calls[0][1].fsPath, canonical);
    assert.equal(app.calls[0][2].forceNewWindow, true);
    assert.equal(app.activations.length, 0);
    assert.equal(app.verifications.length, 0);
    assert.equal(app.pending().provider, 'claude');
    assert.equal(app.pending().id, id);
    assert.equal(app.pending().cwd, canonical);
    assert.ok(Number.isFinite(app.pending().requestedAt));
  }
});

test('startup consumes a recent matching request once', async () => {
  const app = fixture({ pending: { provider: 'codex', id, cwd, requestedAt: Date.now() } });
  await app.ready;
  assert.deepEqual(commandNames(app), ['vscode.openWith']);
  assert.equal(app.pending(), undefined);
});

test('startup does not steal a request in the wrong or multiple-folder workspace', async () => {
  for (const folders of [[other], [cwd, other]]) {
    const app = fixture({ folders, pending: { provider: 'claude', id, cwd, requestedAt: Date.now() } });
    await app.ready;
    assert.equal(app.calls.length, 0);
    assert.equal(app.activations.length, 0);
    assert.equal(app.pending().id, id);
  }
});

test('startup rejects expired, future, and malformed pending requests', async () => {
  for (const changes of [{ requestedAt: Date.now() - 120001 }, { requestedAt: Date.now() + 60000 },
    { requestedAt: 'recent' }, { requestedAt: undefined }, { provider: 'other' }, { id: 'draft' },
    { cwd: 'relative' }, { cwd: regularFile }]) {
    const app = fixture({ pending: { provider: 'claude', id, cwd, requestedAt: Date.now(), ...changes } });
    await app.ready;
    assert.equal(app.calls.length, 0);
    assert.equal(app.activations.length, 0);
    assert.equal(app.pending(), undefined);
  }
});

test('rejects malformed links before provider activation or workspace routing', async () => {
  for (const changes of [{ query: '' }, { query: `id=${id}&cwd=${encodeURIComponent(cwd)}` },
    { query: query('other') }, { query: `${query()}&provider=claude` },
    { query: `${query()}&id=${id}` }, { query: `${query()}&cwd=${encodeURIComponent(cwd)}` },
    { query: `${query()}&prompt=hello` }, { query: query().replace(id, 'draft') },
    { query: query('claude', 'relative') }, { query: query('claude', `${cwd}\0bad`) },
    { query: query('claude', `${cwd}\n`) }, { query: query('claude', regularFile) },
    { query: query('claude', path.join(root, 'missing')) },
    { query: `${query()}%` }, { query: `${query()}%GG` }, { query: `${query()}%C3` },
    { scheme: 'https' }, { authority: 'other.extension' }, { path: '/send' }, { fragment: 'content' }]) {
    const app = fixture();
    await app.open(changes);
    assert.equal(app.calls.length, 0);
    assert.equal(app.activations.length, 0);
    assert.equal(app.errors.length, 1);
  }
});

test('a missing provider reports an error without opening another UI', async () => {
  for (const provider of ['claude', 'codex']) {
    const app = fixture({ missing: true });
    await app.open({ query: query(provider) });
    assert.equal(app.calls.length, 0);
    assert.equal(app.errors.length, 1);
  }
});

test('Claude verification failure prevents the editor command', async () => {
  const app = fixture({ verifyError: new Error('session unavailable') });
  await app.open();
  assert.equal(app.verifications.length, 1);
  assert.equal(app.calls.length, 0);
  assert.match(app.errors[0], /session unavailable/);
});

test('activation and editor command errors are surfaced without fallback', async () => {
  const activation = fixture({ activateError: new Error('activation failed') });
  await activation.open();
  assert.equal(activation.calls.length, 0);
  assert.match(activation.errors[0], /activation failed/);
  const command = fixture({ commandError: new Error('editor failed') });
  await command.open({ query: query('codex') });
  assert.deepEqual(commandNames(command), ['vscode.openWith']);
  assert.match(command.errors[0], /editor failed/);
});

test('failed folder routing clears its pending request', async () => {
  const app = fixture({ folders: [other], folderError: new Error('window failed') });
  await app.open();
  assert.deepEqual(commandNames(app), ['vscode.openFolder']);
  assert.equal(app.pending(), undefined);
  assert.match(app.errors[0], /window failed/);
});

test('two destination requests both survive until their matching windows start', async () => {
  const storage = new Map();
  const origin = fixture({ folders: [], storage });
  await origin.open({ query: query('codex', cwd) });
  await origin.open({ query: query('claude', other) });
  assert.equal(storage.size, 2);
  const first = fixture({ folders: [cwd], storage });
  await first.ready;
  assert.deepEqual(commandNames(first), ['vscode.openWith']);
  assert.equal(storage.size, 1);
  const second = fixture({ folders: [other], storage });
  await second.ready;
  assert.deepEqual(commandNames(second), ['claude-vscode.primaryEditor.open']);
  assert.equal(storage.size, 0);
});

test('opening directly leaves an unrelated destination request pending', async () => {
  const app = fixture();
  await app.open({ query: query('codex', other) });
  const [key, pending] = [...app.storage][0];
  await app.open();
  assert.equal(app.storage.get(key), pending);
  assert.deepEqual(commandNames(app), ['vscode.openFolder', 'claude-vscode.primaryEditor.open']);
});

test('failed destination routing clears only that request', async () => {
  const pending = { provider: 'claude', id, cwd: other, requestedAt: Date.now() };
  const app = fixture({ folders: [], pending, folderError: new Error('window failed') });
  await app.open({ query: query('codex', cwd) });
  assert.equal(app.storage.size, 1);
  assert.equal(app.pending(), pending);
});

test('startup processes every matching request and preserves unrelated state', async () => {
  const storage = new Map([
    ['other-setting', 'keep'],
    ['pendingSession:corrupt', null],
    ['pendingSession:first', { provider: 'codex', id, cwd, requestedAt: Date.now() }],
    ['pendingSession:elsewhere', { provider: 'claude', id, cwd: other, requestedAt: Date.now() }],
    ['pendingSession:second', { provider: 'claude', id, cwd, requestedAt: Date.now() }],
  ]);
  const app = fixture({ storage });
  await app.ready;
  assert.deepEqual(commandNames(app), ['vscode.openWith', 'claude-vscode.primaryEditor.open']);
  assert.deepEqual([...storage.keys()], ['other-setting', 'pendingSession:elsewhere']);
});

test('an unavailable current workspace folder routes to the valid target', async () => {
  const app = fixture({ folders: [path.join(root, 'missing-workspace')] });
  await app.open();
  assert.deepEqual(commandNames(app), ['vscode.openFolder']);
  assert.equal(app.calls[0][1].fsPath, canonical);
});

test('a workspace change during provider activation or verification prevents opening a chat', async () => {
  const changeWorkspace = workspace => {
    workspace.workspaceFolders = [{ uri: { scheme: 'file', fsPath: other } }];
  };
  for (const options of [
    { activateAction: changeWorkspace },
    { verifyAction: changeWorkspace },
  ]) {
    const app = fixture(options);
    await app.open();
    assert.equal(app.calls.length, 0);
    assert.match(app.errors[0], /workspace changed/i);
  }
  const codex = fixture({ activateAction: changeWorkspace });
  await codex.open({ query: query('codex') });
  assert.equal(codex.calls.length, 0);
  assert.match(codex.errors[0], /workspace changed/i);
});
