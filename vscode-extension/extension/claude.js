const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');

const run = promisify(execFile);
const limit = 65536;
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const canonical = async value => {
  const real = await fs.realpath(value);
  return process.platform === 'darwin' ? real.normalize('NFC') : real;
};

function projectKey(cwd, env = {}) {
  const custom = env.CLAUDE_CONFIG_DIR && env.CLAUDE_CODE_PROJECT_DIR_NAME;
  if (custom) {
    if (!/^[A-Za-z0-9_-]{1,64}$/.test(custom) || /^(con|prn|aux|nul|com[0-9]|lpt[0-9])$/i.test(custom))
      throw new Error('Unsupported Claude project directory override.');
    return custom;
  }
  const key = cwd.replace(/[^a-zA-Z0-9]/g, '-');
  if (key.length <= 200) return key;
  let hash = 0;
  for (let index = 0; index < cwd.length; index++) hash = (Math.imul(hash, 31) + cwd.charCodeAt(index)) | 0;
  return `${key.slice(0, 200)}-${Math.abs(hash).toString(36)}`;
}

function environment(settings) {
  const env = { ...process.env };
  const values = settings.get('environmentVariables') || [];
  const entries = Array.isArray(values) ? values : Object.entries(values).map(([name, value]) => ({ name, value }));
  for (const entry of entries) {
    if (!entry || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(entry.name)) continue;
    const value = entry.value == null ? '' : String(entry.value);
    if (entry.name === 'CLAUDE_CONFIG_DIR' && !path.isAbsolute(value)) continue;
    env[entry.name] = value;
  }
  env.CLAUDE_CODE_ENTRYPOINT = 'claude-vscode';
  delete env.CLAUDECODE;
  delete env.CLAUDE_CODE_CHILD_SESSION;
  delete env.TRACEPARENT;
  delete env.TRACESTATE;
  return env;
}

async function transcript(file) {
  let handle;
  try {
    // Match Claude's bounded metadata read and refuse symlinks, directories, and pipes.
    if (!(await fs.lstat(file)).isFile()) throw new Error();
    handle = await fs.open(file, constants.O_RDONLY | (constants.O_NOFOLLOW || 0) | (constants.O_NONBLOCK || 0));
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size === 0) throw new Error();
    const head = Buffer.alloc(Math.min(limit, stat.size));
    if ((await handle.read(head, 0, head.length, 0)).bytesRead !== head.length) throw new Error();
    if (stat.size <= limit) return [head.toString('utf8'), ''];
    const tail = Buffer.alloc(limit);
    if ((await handle.read(tail, 0, limit, stat.size - limit)).bytesRead !== limit) throw new Error();
    const start = head.toString('utf8'), end = tail.toString('utf8');
    // Native list filters inspect fields even when the sampled row is incomplete.
    if (/"entrypoint"\s*:\s*"(?:sdk-cli|sdk-ts|sdk-py)"|"sessionKind"\s*:\s*"(?:daemon|daemon-worker)"|"isSidechain"\s*:\s*true|"type"\s*:\s*"(?:continued-in|relocated)"/.test(`${start}\n${end}`)) throw new Error();
    return [start.slice(0, start.lastIndexOf('\n') + 1), end.slice(end.indexOf('\n') + 1)];
  } catch {
    throw new Error('Claude session transcript is missing or unreadable.');
  } finally {
    await handle?.close();
  }
}

async function verifySession(vscode, extension, target) {
  if (!uuid.test(target.id) || !path.isAbsolute(target.cwd)) throw new Error('Invalid Claude session target.');
  const settings = vscode.workspace.getConfiguration('claudeCode');
  if (settings.get('claudeProcessWrapper')) throw new Error('Claude process wrappers cannot be verified safely.');
  const env = environment(settings), cwd = await canonical(target.cwd);
  const binary = path.join(extension.extensionPath, 'resources', 'native-binary', process.platform === 'win32' ? 'claude.exe' : 'claude');
  let auth;
  try {
    const { stdout } = await run(binary, ['auth', 'status', '--json'], { cwd, env, timeout: 5000, maxBuffer: limit, windowsHide: true })
      .catch(error => {
        // Native auth status also reports store paths when signed out (exit 1).
        if (error.code === 1 && !error.signal && !error.killed) return { stdout: error.stdout };
        throw error;
      });
    auth = JSON.parse(stdout);
  } catch {
    throw new Error('Could not verify the Claude session store.');
  }
  if (!auth || typeof auth.projectsDirectory !== 'string' || !path.isAbsolute(auth.projectsDirectory) ||
      typeof auth.configDirectory !== 'string' || !path.isAbsolute(auth.configDirectory) ||
      path.resolve(auth.projectsDirectory) !== path.resolve(auth.configDirectory, 'projects'))
    throw new Error('Claude did not report a verifiable session store.');
  const file = path.join(auth.projectsDirectory, projectKey(cwd, env), `${target.id}.jsonl`);
  const [head, tail] = await transcript(file);
  let found = false;
  const firstLines = head.split('\n');
  for (const [index, line] of [...firstLines, ...tail.split('\n')].entries()) {
    if (!line.trim()) continue;
    let row;
    try { row = JSON.parse(line); } catch { throw new Error('Claude session metadata is incomplete.'); }
    if (!row || typeof row !== 'object' || row.isSidechain === true ||
        ['sdk-cli', 'sdk-ts', 'sdk-py'].includes(row.entrypoint) ||
        ['daemon', 'daemon-worker'].includes(row.sessionKind) || ['continued-in', 'relocated'].includes(row.type) ||
        (row.sessionId !== undefined && row.sessionId !== target.id))
      throw new Error('Claude session is not available as an ordinary local conversation.');
    if (row.cwd !== undefined && (typeof row.cwd !== 'string' || await canonical(row.cwd) !== cwd))
      throw new Error('Claude session belongs to another workspace.');
    if (index >= firstLines.length || row.type !== 'user' || row.sessionId !== target.id || row.cwd === undefined || row.isMeta || row.isCompactSummary) continue;
    const content = row.message?.content;
    if (Array.isArray(content) && content.some(block => block?.type === 'tool_result')) continue;
    const texts = typeof content === 'string' ? [content] : Array.isArray(content)
      ? content.filter(block => block?.type === 'text').map(block => block.text) : [];
    if (texts.some(text => typeof text === 'string' && text.trim() && !/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(text) &&
        !/^(?:<[a-z][\w-]*[\s>]|\[Request interrupted by user)/.test(text.trim())) && /"type": ?"user"/.test(line)) found = true;
  }
  if (!found) throw new Error('Claude session has no verifiable persisted user message.');
}

module.exports = { verifySession, projectKey };
