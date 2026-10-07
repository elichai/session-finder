const vscode = require('vscode');
const path = require('node:path');

exports.activate = async context => {
  const output = vscode.window.createOutputChannel('Session Finder', { log: true });
  output.info('Cursor session opener ready');
  const pendingKey = 'pendingSession';
  const session = (id, cwd) => {
    if (typeof id !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)
        || typeof cwd !== 'string' || !path.isAbsolute(cwd) || /[\x00-\x1f\x7f]/.test(cwd)) {
      throw new Error('Invalid Cursor session link.');
    }
    return { id: id.toLowerCase(), cwd: path.resolve(cwd) };
  };
  const matches = target => vscode.workspace.workspaceFolders?.some(folder =>
    folder.uri.scheme === 'file' && path.resolve(folder.uri.fsPath) === target.cwd);
  const report = error => {
    const message = `Session Finder: ${error?.message || String(error)}`;
    output.error(message);
    vscode.window.showErrorMessage(message);
  };
  const open = async target => {
    await context.globalState.update(pendingKey, undefined);
    output.info(`Opening Cursor session ${target.id} in ${target.cwd}`);
    const commands = await vscode.commands.getCommands(true);
    if (commands.includes('glass.openAgentById')) {
      if (await vscode.commands.executeCommand('glass.openAgentById', target.id) !== true) {
        throw new Error(`Cursor could not open session ${target.id}. The chat may no longer exist.`);
      }
      output.info(`Opened Cursor session ${target.id}`);
    } else {
      await vscode.commands.executeCommand('_workbench.open', `cursor.agent://local/${target.id}`);
      output.info(`Requested Cursor session ${target.id} in classic mode; Cursor may show its transcript if the chat is unavailable.`);
    }
  };
  context.subscriptions.push(output, vscode.window.registerUriHandler({
    async handleUri(uri) {
      try {
        const params = new URLSearchParams(uri.query);
        const ids = params.getAll('id');
        const directories = params.getAll('cwd');
        if (uri.scheme !== 'cursor' || uri.authority !== 'session-finder.cursor-session-opener'
            || uri.path !== '/open' || uri.fragment || ids.length !== 1
            || directories.length !== 1 || [...params.keys()].some(key => key !== 'id' && key !== 'cwd')) {
          throw new Error('Invalid Cursor session link.');
        }
        const target = session(ids[0], directories[0]);
        if (matches(target)) {
          await open(target);
        } else {
          await context.globalState.update(pendingKey, { ...target, requestedAt: Date.now() });
          output.info(`Switching to session directory ${target.cwd}`);
          try {
            await vscode.commands.executeCommand('vscode.openFolder', vscode.Uri.file(target.cwd), { forceReuseWindow: true });
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
  try {
    const pending = context.globalState.get(pendingKey);
    if (pending) {
      const age = Date.now() - pending.requestedAt;
      if (!Number.isFinite(age) || age < 0 || age > 120000) {
        await context.globalState.update(pendingKey, undefined);
        return;
      }
      const target = session(pending.id, pending.cwd);
      if (matches(target)) await open(target);
    }
  } catch (error) {
    await context.globalState.update(pendingKey, undefined);
    report(error);
  }
};
