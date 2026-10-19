const vscode = require('vscode');
const fs = require('node:fs/promises');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { verifySession } = require('./claude');

exports.activate = async context => {
  const output = vscode.window.createOutputChannel('Session Finder', { log: true });
  const pendingPrefix = 'pendingSession:';
  const target = async value => {
    if (!value || !['claude', 'codex'].includes(value.provider)
        || typeof value.id !== 'string' || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value.id)
        || typeof value.cwd !== 'string' || !path.isAbsolute(value.cwd) || /\p{Cc}/u.test(value.cwd)) {
      throw new Error('Invalid session link.');
    }
    const cwd = await fs.realpath(value.cwd);
    if (/\p{Cc}/u.test(cwd) || !(await fs.stat(cwd)).isDirectory()) throw new Error('Session workspace is not a valid directory.');
    return { provider: value.provider, id: value.id.toLowerCase(), cwd };
  };
  const matches = async value => {
    const folders = vscode.workspace.workspaceFolders;
    // Both providers derive their working directory from the first workspace folder.
    if (folders?.length !== 1 || folders[0].uri.scheme !== 'file') return false;
    try { return await fs.realpath(folders[0].uri.fsPath) === value.cwd; }
    catch { return false; }
  };
  const report = error => {
    const message = `Session Finder: ${error?.message || String(error)}`;
    output.error(message);
    vscode.window.showErrorMessage(message);
  };
  const open = async value => {
    const name = value.provider === 'claude' ? 'anthropic.claude-code' : 'openai.chatgpt';
    const extension = vscode.extensions.getExtension(name);
    if (!extension) throw new Error(`Install the ${name} extension in this VS Code profile first.`);
    await extension.activate();
    if (value.provider === 'claude') await verifySession(vscode, extension, value);
    if (!await matches(value)) throw new Error('The workspace changed while preparing the session. Open the session link again.');
    if (value.provider === 'claude') {
      await vscode.commands.executeCommand('claude-vscode.primaryEditor.open', value.id, undefined);
    } else {
      await vscode.commands.executeCommand('vscode.openWith',
        vscode.Uri.parse(`openai-codex://route/local/${value.id}`),
        'chatgpt.conversationEditor', { preview: false, preserveFocus: false });
    }
    output.info(`Requested ${value.provider} session ${value.id} in ${value.cwd}`);
  };
  context.subscriptions.push(output, vscode.window.registerUriHandler({
    async handleUri(uri) {
      try {
        if (uri.scheme !== vscode.env.uriScheme || uri.authority !== 'session-finder.vscode-session-opener'
            || uri.path !== '/open' || uri.fragment) throw new Error('Invalid session link.');
        decodeURIComponent(uri.query.replace(/\+/g, ' '));
        const params = new URLSearchParams(uri.query);
        const keys = ['provider', 'id', 'cwd'];
        if (keys.some(key => params.getAll(key).length !== 1)
            || [...params.keys()].some(key => !keys.includes(key))) throw new Error('Invalid session link.');
        const value = await target(Object.fromEntries(params));
        if (await matches(value)) {
          await open(value);
        } else {
          const pendingKey = pendingPrefix + randomUUID();
          await context.globalState.update(pendingKey, { ...value, requestedAt: Date.now() });
          try {
            await vscode.commands.executeCommand('vscode.openFolder', vscode.Uri.file(value.cwd), { forceNewWindow: true });
          } catch (error) {
            await context.globalState.update(pendingKey, undefined);
            throw error;
          }
        }
      } catch (error) {
        report(error);
      }
    },
  }));
  for (const pendingKey of context.globalState.keys().filter(key => key.startsWith(pendingPrefix))) {
    try {
      const pending = context.globalState.get(pendingKey);
      const age = Date.now() - pending?.requestedAt;
      if (typeof pending?.requestedAt !== 'number' || !Number.isFinite(age) || age < 0 || age > 120000) {
        await context.globalState.update(pendingKey, undefined);
        continue;
      }
      const value = await target(pending);
      if (await matches(value)) {
        await context.globalState.update(pendingKey, undefined);
        await open(value);
      }
    } catch (error) {
      await context.globalState.update(pendingKey, undefined);
      report(error);
    }
  }
};
