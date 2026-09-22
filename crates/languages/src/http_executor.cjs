'use strict';

const fs = require('node:fs');
const path = require('node:path');
const net = require('node:net');
const crypto = require('node:crypto');
const childProcess = require('node:child_process');
const { createRequire } = require('node:module');
const { pathToFileURL } = require('node:url');
const { format } = require('node:util');

const delay = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
const owner = Number(process.env.ZED_CUSTOM_HTTPYAC_OWNER);
const sessionDirectory = path.dirname(__filename);
const MAX_MESSAGE = 2 * 1024 * 1024;
const machine = process.argv.includes('--json') && process.argv[2] !== '--server';
let machineConnection;
let machineClosed = false;

function emit(message) {
    process.stdout.write(JSON.stringify(message) + '\n');
}

function bounded(value, length = 16384) {
    const text = typeof value === 'string' ? value : JSON.stringify(value) || '';
    return text.length > length ? text.slice(0, length) + '\n[preview truncated]' : text;
}

function responseEvent(response, region) {
    const raw = response.rawBody || (Buffer.isBuffer(response.body) ? response.body : Buffer.from(typeof response.body === 'string' ? response.body : JSON.stringify(response.body) || ''));
    const limit = 512 * 1024;
    const request = response.request;
    const contentType = String(response.headers?.['content-type'] || '');
    const binary = contentType && !/^(?:text\/|application\/(?:[^;]*json|[^;]*xml|javascript|x-www-form-urlencoded))/i.test(contentType);
    const text = binary ? '[Binary response — use Save Body to export the bytes]' : typeof response.body === 'string' ? response.body : raw.toString('utf8');
    const headers = response.rawHeaders
        ? response.rawHeaders.reduce((lines, value, index, values) => { if (index % 2 === 0) lines.push(`${value}: ${values[index + 1] || ''}`); return lines; }, []).join('\n')
        : Object.entries(response.headers || {}).flatMap(([name, value]) => (Array.isArray(value) ? value : [value]).map(value => `${name}: ${value}`)).join('\n');
    return {
        type: 'response', name: bounded(region?.metaData?.name || region?.symbol?.name || 'Response', 512),
        status: response.statusCode, statusMessage: bounded(response.statusMessage || '', 512),
        headers: bounded(headers),
        request: bounded(request ? `${request.method || ''} ${request.url}\n${JSON.stringify(request.headers || {}, null, 2)}\n\n${typeof request.body === 'string' ? request.body : JSON.stringify(request.body) || ''}` : ''),
        body: bounded(text, 128 * 1024),
        rawBody: raw.subarray(0, limit).toString('base64'), bodyBytes: raw.length,
        truncated: raw.length > limit, textTruncated: text.length > 128 * 1024,
        contentType: bounded(contentType, 512), timings: response.timings || {},
    };
}

function alive(pid) {
    if (!Number.isInteger(pid) || pid <= 0) return false;
    try { process.kill(pid, 0); return true; }
    catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}

function writeRecord(directory, value) {
    const temporary = path.join(directory, `record-${process.pid}.tmp`);
    fs.writeFileSync(temporary, JSON.stringify(value), { mode: 0o600 });
    fs.renameSync(temporary, path.join(directory, 'endpoint.json'));
}

function readRecord(directory) {
    try { return JSON.parse(fs.readFileSync(path.join(directory, 'endpoint.json'), 'utf8')); }
    catch (error) {
        if (error.code === 'ENOENT' || error instanceof SyntaxError) return undefined;
        throw error;
    }
}

function removeOwnRecord(directory, nonce) {
    if (readRecord(directory)?.nonce === nonce) fs.rmSync(directory, { recursive: true, force: true });
}

function frames(socket, handle) {
    let pending = '';
    socket.setEncoding('utf8');
    socket.on('data', data => {
        pending += data;
        let boundary;
        while ((boundary = pending.indexOf('\n')) >= 0) {
            const message = pending.slice(0, boundary);
            pending = pending.slice(boundary + 1);
            if (Buffer.byteLength(message) > MAX_MESSAGE) { socket.destroy(new Error('Executor message too large')); return; }
            try { handle(JSON.parse(message)); }
            catch (error) { socket.destroy(error); return; }
        }
        if (Buffer.byteLength(pending) > MAX_MESSAGE) socket.destroy(new Error('Executor message too large'));
    });
}

function transmit(socket, message) {
    if (socket.destroyed) return;
    if (socket.writableLength > 32 * 1024 * 1024) {
        socket.destroy(new Error('Executor output consumer is too slow'));
        return;
    }
    socket.write(JSON.stringify(message) + '\n');
}

function findEngine(root) {
    const candidates = [];
    if (process.env.ZED_HTTPYAC_MODULE) candidates.push(path.resolve(process.env.ZED_HTTPYAC_MODULE));
    else {
        for (const directory of (process.env.PATH || process.env.Path || '').split(path.delimiter)) {
            if (!directory) continue;
            for (const name of process.platform === 'win32' ? ['httpyac.cmd', 'httpyac', 'httpyac.ps1'] : ['httpyac']) {
                const launcher = path.join(directory, name);
                try {
                    const actual = fs.realpathSync(launcher);
                    candidates.push(path.resolve(path.dirname(actual), '..'));
                    candidates.push(path.join(directory, 'node_modules', 'httpyac'));
                    candidates.push(path.resolve(directory, '..', 'httpyac'));
                } catch (error) { if (!['ENOENT', 'ENOTDIR'].includes(error.code)) throw error; }
            }
        }
        try { candidates.push(path.dirname(createRequire(path.join(root, 'package.json')).resolve('httpyac/package.json'))); }
        catch (error) { if (error.code !== 'MODULE_NOT_FOUND') throw error; }
        candidates.push(path.join(path.dirname(process.execPath), 'node_modules', 'httpyac'));
        candidates.push(path.resolve(path.dirname(process.execPath), '..', 'lib', 'node_modules', 'httpyac'));
    }
    for (const directory of candidates) {
        try {
            const manifestPath = path.join(directory, 'package.json');
            const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
            if (manifest.name !== 'httpyac') continue;
            if (!/^6\./.test(manifest.version)) throw new Error(`Unsupported httpyac ${manifest.version}; this bridge supports httpyac 6.x (tested with 6.16.7).`);
            return { directory: fs.realpathSync(directory), version: manifest.version };
        } catch (error) { if (!['ENOENT', 'ENOTDIR'].includes(error.code)) throw error; }
    }
    throw new Error('Cannot locate the installed httpyac Node module. Install httpyac in the project or globally, or set ZED_HTTPYAC_MODULE to its package directory.');
}

function environment() {
    const value = process.env.ZED_HTTPYAC_ENV || '';
    const environments = value.startsWith('[') ? JSON.parse(value) : value.split(',').map(value => value.trim()).filter(Boolean);
    if (!Array.isArray(environments) || environments.some(value => typeof value !== 'string')) throw new Error('ZED_HTTPYAC_ENV must be a comma-separated list or a JSON string array.');
    return environments;
}

function configuration(reset) {
    if (!alive(owner)) throw new Error('The owning Zed/remote server process is no longer running.');
    const root = fs.realpathSync(process.env.ZED_WORKTREE_ROOT || process.cwd());
    if (reset) return { root };
    const engine = findEngine(root);
    const environments = environment();
    // Fresh task shells/SSH connections get new prompt and terminal identifiers. These
    // must not split a project session, unlike actual credentials or endpoint settings.
    // Keep this list explicit: e.g. SSH_AUTH_SOCK and STARSHIP_CONFIG still matter.
    const transientEnvironment = new Set([
        'PWD', 'OLDPWD', 'SHLVL', '_',
        'STARSHIP_SESSION_KEY', 'TERM_SESSION_ID', 'WT_SESSION', 'WINDOWID',
        'SSH_CLIENT', 'SSH_CONNECTION', 'SSH_TTY', 'TTY', 'COLUMNS', 'LINES',
    ]);
    const processEnvironment = Object.entries(process.env)
        .filter(([name]) => !name.startsWith('ZED_') && !transientEnvironment.has(name))
        .sort(([left], [right]) => left.localeCompare(right));
    const key = digest(JSON.stringify([root, engine, environments, processEnvironment]));
    return { root, engine, environments, directory: path.join(sessionDirectory, key) };
}

async function connect(record) {
    return new Promise((resolve, reject) => {
        const socket = net.createConnection({ host: '127.0.0.1', port: record.port });
        const timer = setTimeout(() => socket.destroy(new Error('Executor connection timed out')), 2000);
        socket.once('error', reject);
        socket.once('connect', () => {
            clearTimeout(timer);
            socket.removeListener('error', reject);
            resolve(socket);
        });
        socket.once('close', () => clearTimeout(timer));
    });
}

async function session(config, start) {
    const deadline = Date.now() + 15000;
    while (Date.now() < deadline) {
        const record = readRecord(config.directory);
        if (record && !alive(record.pid)) {
            // A starter may have died just after spawning its child, before the child
            // published its own PID. Give that child time to claim the startup record.
            if (!record.port && Date.now() - record.created < 5000) { await delay(40); continue; }
            removeOwnRecord(config.directory, record.nonce);
            continue;
        }
        if (record?.port) return { record, socket: await connect(record) };
        if (!start && !record) return undefined;
        try {
            fs.mkdirSync(config.directory, { mode: 0o700 });
            const nonce = crypto.randomBytes(32).toString('hex');
            writeRecord(config.directory, { pid: process.pid, nonce, created: Date.now() });
            const worker = childProcess.spawn(process.execPath, [__filename, '--server', JSON.stringify({ ...config, nonce })], {
                cwd: config.root, env: process.env, detached: true, stdio: 'ignore', windowsHide: true,
            });
            await new Promise((resolve, reject) => { worker.once('spawn', resolve); worker.once('error', reject); });
            worker.unref();
        } catch (error) {
            if (error.code !== 'EEXIST') throw error;
            // A killed starter can leave an empty directory before publishing its PID.
            if (!record) {
                try {
                    if (Date.now() - fs.statSync(config.directory).mtimeMs > 15000 && !readRecord(config.directory)) fs.rmSync(config.directory, { recursive: true, force: true });
                } catch (error) { if (error.code !== 'ENOENT') throw error; }
            }
        }
        await delay(40);
    }
    throw new Error('HTTP execution session did not start; retry the task or restart Zed.');
}

async function prompt(engine, message) {
    if (!process.stdin.isTTY) throw new Error('This request requires interactive input; run it in an interactive terminal.');
    const resolve = createRequire(path.join(engine.directory, 'package.json'));
    const inquirer = (await import(pathToFileURL(resolve.resolve('inquirer')).href)).default;
    const answer = await inquirer.prompt([{
        name: 'value', type: message.kind, message: message.message, default: message.defaultValue,
        choices: message.choices, mask: message.kind === 'password' ? '*' : undefined,
    }]);
    return answer.value;
}

async function exchange(connection, request, engine) {
    const { socket, record } = connection;
    if (machine) {
        if (machineClosed) { socket.destroy(); throw new Error('Response view disconnected before execution'); }
        machineConnection = socket;
    }
    return new Promise((resolve, reject) => {
        let finished = false;
        const interrupt = () => { socket.destroy(); process.exitCode = 130; };
        process.once('SIGINT', interrupt);
        process.once('SIGTERM', interrupt);
        socket.on('error', reject);
        socket.on('close', () => {
            process.removeListener('SIGINT', interrupt);
            process.removeListener('SIGTERM', interrupt);
            if (!finished) reject(new Error('Execution session disconnected. The request may have been sent; it will NOT be retried automatically.'));
        });
        frames(socket, message => {
            if (machine && !['done', 'restart'].includes(message.type)) {
                if (!process.stdout.write(JSON.stringify(message) + '\n')) {
                    socket.pause();
                    process.stdout.once('drain', () => socket.resume());
                }
            } else if (message.type === 'output') {
                const output = message.stream === 'stderr' ? process.stderr : process.stdout;
                if (!output.write(message.text)) {
                    socket.pause();
                    output.once('drain', () => socket.resume());
                }
            } else if (message.type === 'prompt') {
                prompt(engine, message).then(value => transmit(socket, { answer: message.id, value }), error => transmit(socket, { answer: message.id, error: error.message }));
            } else if (message.type === 'done' || message.type === 'restart') {
                finished = true;
                socket.end();
                resolve(message);
            } else throw new Error('Invalid execution response');
        });
        // Never retry after this write unless the server explicitly reports that preflight
        // found changed inputs and no request or script has started yet.
        transmit(socket, { token: record.token, ...request });
    });
}

async function client() {
    if (machine) {
        let lastHeartbeat = Date.now();
        frames(process.stdin, message => {
            lastHeartbeat = Date.now();
            if (message.type === 'ping') return;
            if (message.type === 'cancel') { machineClosed = true; machineConnection?.destroy(); return; }
            if (typeof message.answer !== 'string') throw new Error('Invalid response view input');
            if (machineConnection) transmit(machineConnection, message);
        });
        process.stdin.on('end', () => { machineClosed = true; machineConnection?.destroy(); });
        process.stdin.on('error', () => { machineClosed = true; machineConnection?.destroy(); });
        // A broken remote transport must not leave an invisible request or prompt running.
        const heartbeat = setInterval(() => {
            if (Date.now() - lastHeartbeat > 15000) { machineConnection?.destroy(); process.exit(130); }
        }, 2000);
        heartbeat.unref();
    }
    const args = process.argv.slice(1);
    const operation = args[0];
    if (!['send', 'reset'].includes(operation)) throw new Error('Expected send or reset');
    const outputIndex = args.indexOf('--output');
    const output = outputIndex < 0 ? 'response' : args[outputIndex + 1];
    if (!['response', 'body', 'headers', 'exchange'].includes(output)) throw new Error('Invalid output format');
    const config = configuration(operation === 'reset');
    if (operation === 'reset') {
        // Reset all environment variants of this project, without loading the engine.
        for (const entry of fs.readdirSync(sessionDirectory, { withFileTypes: true })) {
            if (!entry.isDirectory()) continue;
            const directory = path.join(sessionDirectory, entry.name);
            const record = readRecord(directory);
            if (!record || record.root !== config.root) continue;
            if (!alive(record.pid)) { removeOwnRecord(directory, record.nonce); continue; }
            const result = await exchange({ record, socket: await connect(record) }, { operation: 'reset' }, config.engine);
            if (result.code !== 0) throw new Error('Session reset failed');
        }
        if (machine) emit({ type: 'output', stream: 'stdout', text: 'HTTP execution sessions reset for this project.\n' });
        else console.log('HTTP execution sessions reset for this project.');
        return;
    }
    const file = fs.realpathSync(process.env.ZED_FILE || '');
    const line = args.includes('--all') ? undefined : Number(process.env.ZED_ROW);
    if (line !== undefined && (!Number.isInteger(line) || line < 1)) throw new Error('ZED_ROW must be a one-based line number.');
    for (let attempt = 0; attempt < 3; attempt++) {
        const connection = await session(config, true);
        const result = await exchange(connection, { operation, file, line, output, structured: machine }, config.engine);
        if (result.type === 'restart') { await delay(80); continue; }
        process.exitCode = result.code;
        return;
    }
    throw new Error('Request inputs kept changing during preflight. Nothing was sent by the last attempt.');
}

function fingerprint(file) {
    try {
        if (fs.statSync(file).isDirectory()) return 'directory';
        return digest(fs.readFileSync(file));
    }
    catch (error) { if (['ENOENT', 'ENOTDIR', 'EISDIR'].includes(error.code)) return null; throw error; }
}

async function server(config) {
    if (readRecord(config.directory)?.nonce !== config.nonce) return;
    let active;
    let queue = Promise.resolve();
    let engine;
    let store;
    let lastUsed = Date.now();
    const tracked = new Map();
    const manifests = new Map();
    const documents = new Map();
    const documentReads = new Set();
    const externalInputs = new Set();
    const token = crypto.randomBytes(32).toString('hex');
    let closing = false;
    const shutdown = () => {
        closing = true;
        removeOwnRecord(config.directory, config.nonce);
        process.exit(0);
    };
    const output = (stream, text) => {
        if (active) {
            for (let offset = 0; offset < text.length;) {
                let end = Math.min(offset + 16384, text.length);
                // Splitting a UTF-16 surrogate pair would corrupt Unicode at the client.
                if (end < text.length && /[\uD800-\uDBFF]/.test(text[end - 1])) end--;
                transmit(active.socket, { type: 'output', stream, text: text.slice(offset, end) });
                offset = end;
            }
        }
    };
    // Route console output from user scripts/plugins too, never to a daemon log containing tokens.
    for (const stream of ['stdout', 'stderr']) process[stream].write = (chunk, encoding, callback) => {
        output(stream, Buffer.isBuffer(chunk) ? chunk.toString(typeof encoding === 'string' ? encoding : 'utf8') : String(chunk));
        if (typeof encoding === 'function') encoding();
        else if (callback) callback();
        return true;
    };
    const track = file => { const resolved = path.resolve(String(file)); tracked.set(resolved, fingerprint(resolved)); };
    const configSnapshot = directory => {
        const result = [];
        for (let current = directory;; current = path.dirname(current)) {
            for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
                if ((entry.isFile() || entry.isSymbolicLink()) && (entry.name.startsWith('.env') || /^(?:\.?httpyac|\.httpyac\.config|http-client).*\.(?:json|js|cjs)$/.test(entry.name) || entry.name === 'package.json')) {
                    const file = path.join(current, entry.name);
                    result.push([file, fingerprint(file)]);
                }
            }
            if (current === config.root || path.dirname(current) === current) break;
        }
        return JSON.stringify(result.sort(([left], [right]) => left.localeCompare(right)));
    };
    const changed = () => {
        for (const [file, before] of tracked) {
            // HTTP documents are reparsed through HttpFileStore below, which preserves
            // variables for unchanged request blocks rather than losing the whole session.
            if ((!documents.has(file) || externalInputs.has(file)) && fingerprint(file) !== before) return true;
        }
        for (const [directory, before] of manifests) if (configSnapshot(directory) !== before) return true;
        return false;
    };
    async function invalidateHttpFile(previous, current) {
        const previousSources = new Set(previous.httpRegions.map(region => region.symbol.source));
        const currentSources = new Set(current.httpRegions.map(region => region.symbol.source));
        const modified = [
            ...previous.httpRegions.filter(region => !currentSources.has(region.symbol.source)),
            ...current.httpRegions.filter(region => !previousSources.has(region.symbol.source)),
        ];
        const regions = store.getAll().flatMap(file => file.httpRegions);
        const clear = region => {
            for (const key of Object.keys(region.variablesPerEnv)) delete region.variablesPerEnv[key];
            delete region.response;
            delete region.testResults;
        };
        const sharedSources = file => JSON.stringify(file.httpRegions
            .filter(region => region.isGlobal() || region.symbol.filter(symbol => ['script', 'variableDefinition'].includes(symbol.kind)).length)
            .map(region => region.symbol.source));
        if (sharedSources(previous) !== sharedSources(current)) {
            // Scripts and variable declarations can export arbitrary names or mutate
            // $global. Do not attempt to infer their effects from request names.
            for (const region of regions) clear(region);
            await engine.store.userSessionStore.reset();
            return;
        }
        const invalidNames = new Map();
        const addNames = region => {
            const name = region.metaData.name;
            if (typeof name !== 'string') return;
            for (const variable of [name, name + 'Response']) {
                const escaped = variable.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
                invalidNames.set(variable, new RegExp(`(?<![\\p{L}\\p{N}_$])${escaped}(?![\\p{L}\\p{N}_$])`, 'u'));
            }
        };
        for (const region of modified) addNames(region);
        const invalidated = new Set();
        let expanded;
        do {
            expanded = false;
            for (const region of regions) {
                if (invalidated.has(region)) continue;
                const names = Object.values(region.variablesPerEnv).flatMap(variables => Object.keys(variables));
                if (![...invalidNames].some(([name, pattern]) => names.includes(name) || pattern.test(region.symbol.source || ''))) continue;
                // @ref copies its dependency's variables into the caller's cache. Keeping
                // those copies would resurrect an edited/deleted request's old response.
                clear(region);
                addNames(region);
                invalidated.add(region);
                expanded = true;
            }
        } while (expanded);
    }
    async function initialize() {
        engine = require(config.engine.directory);
        if (!engine.cli?.initIOProvider || !engine.store?.HttpFileStore || !engine.send) throw new Error('Installed httpyac lacks the required library API.');
        await engine.cli.initIOProvider();
        for (const method of ['readFile', 'readBuffer', 'exists']) {
            const original = engine.io.fileProvider[method];
            engine.io.fileProvider[method] = async (...args) => {
                track(args[0]);
                const filename = path.resolve(String(args[0]));
                if (method === 'readBuffer' || (method === 'readFile' && !documentReads.has(filename))) externalInputs.add(filename);
                return original(...args);
            };
        }
        const ask = (kind, message, defaultValue, choices) => {
            if (!active) throw new Error('No active request for user interaction');
            const id = crypto.randomUUID();
            return new Promise((resolve, reject) => {
                active.prompts.set(id, { resolve, reject });
                transmit(active.socket, { type: 'prompt', id, kind, message, defaultValue, choices });
            });
        };
        Object.assign(engine.io.userInteractionProvider, {
            showNote: message => ask('confirm', message),
            showInputPrompt: (message, defaultValue, masked) => ask(masked ? 'password' : 'input', message, defaultValue),
            showListPrompt: (message, choices) => ask('list', message, undefined, choices),
        });
        engine.io.log.options.logMethod = (_level, ...args) => output('stderr', format(...args) + '\n');
        store = new engine.store.HttpFileStore();
        const original = store.getOrCreate.bind(store);
        store.getOrCreate = async (file, getText, _version, options) => {
            const filename = path.resolve(String(file));
            track(filename);
            manifests.set(path.dirname(filename), configSnapshot(path.dirname(filename)));
            let text;
            documentReads.add(filename);
            try { text = await getText(); }
            finally { documentReads.delete(filename); }
            const previous = documents.get(filename);
            const version = previous?.text === text ? previous.version : (previous?.version || 0) + 1;
            const httpFile = await original(filename, async () => text, version, options);
            if (previous && previous.text !== text) await invalidateHttpFile(previous.httpFile, httpFile);
            documents.set(filename, { version, text, httpFile, options });
            return httpFile;
        };
    }
    async function execute(message, connection) {
        if (closing || connection.destroyed) return;
        active = { socket: connection, prompts: new Map(), canceled: false, cancellations: new Set() };
        try {
            if (message.operation !== 'send' || typeof message.file !== 'string' || !path.isAbsolute(message.file) || !['response', 'body', 'headers', 'exchange'].includes(message.output)) throw new Error('Invalid execution request');
            if (message.line !== undefined && (!Number.isInteger(message.line) || message.line < 1)) throw new Error('Invalid line number');
            if (changed()) {
                closing = true;
                removeOwnRecord(config.directory, config.nonce);
                connection.end(JSON.stringify({ type: 'restart' }) + '\n', shutdown);
                return;
            }
            if (!engine) await initialize();
            // Refresh previously imported files before send() assembles context.variables;
            // otherwise an importer could consume stale values before @import runs.
            for (const [filename, document] of [...documents]) {
                const text = await fs.promises.readFile(filename, 'utf8');
                if (text !== document.text) await store.getOrCreate(filename, async () => text, 0, document.options);
            }
            const httpFile = await store.getOrCreate(message.file, () => fs.promises.readFile(message.file, 'utf8'), 0, { workingDir: config.root });
            const context = {
                httpFile, activeEnvironment: config.environments, processedHttpRegions: [],
                scriptConsole: new engine.io.Logger({ logMethod: (_level, ...args) => output('stdout', format(...args) + '\n') }),
                progress: { isCanceled: () => active?.canceled ?? true, register: callback => { const callbacks = active.cancellations; callbacks.add(callback); return () => callbacks.delete(callback); } },
                logResponse: async (response, region) => {
                    if (response) {
                        if (message.structured) {
                            transmit(connection, responseEvent(response, region));
                            return;
                        }
                        output('stdout', `\n${region?.metaData?.name || region?.symbol?.name || 'Response'}\n`);
                        const options = { responseBodyPrettyPrint: true, responseHeaders: message.output !== 'body', responseBodyLength: message.output === 'headers' ? undefined : 0, requestOutput: message.output === 'exchange', requestHeaders: message.output === 'exchange', requestBodyLength: message.output === 'exchange' ? 0 : undefined };
                        await engine.utils.requestLoggerFactory(text => output('stdout', text + '\n'), options)(response, region);
                    }
                },
                logStream: async (type, response) => output('stdout', `${type}: ${typeof response.body === 'string' ? response.body : JSON.stringify(response.body)}\n`),
            };
            if (message.line !== undefined) {
                const line = message.line - 1;
                context.httpRegion = httpFile.httpRegions.find(region => !region.isGlobal() && region.symbol.startLine <= line && region.symbol.endLine >= line);
                if (!context.httpRegion) throw new Error(`No request at line ${message.line}; select its request line.`);
            }
            if (message.structured) {
                const regions = context.httpRegion ? [context.httpRegion] : httpFile.httpRegions.filter(region => !region.isGlobal());
                transmit(connection, { type: 'started', streaming: regions.some(region => ['WS', 'SSE'].includes(region.request?.protocol)) });
            }
            const success = await engine.send(context);
            const tests = context.processedHttpRegions.flatMap(region => region.testResults || []);
            for (const test of tests) {
                if (message.structured) transmit(connection, { type: 'test', status: bounded(test.status, 128), message: bounded(test.message) });
                else if (['ERROR', 'FAILED'].includes(test.status)) output('stderr', `${test.status}: ${test.message}\n`);
            }
            const failed = !success || tests.some(test => ['ERROR', 'FAILED'].includes(test.status));
            // Include modules loaded by request scripts/configuration in the next preflight.
            for (const filename of Object.keys(require.cache)) {
                if (!filename.includes(`${path.sep}node_modules${path.sep}`) && !filename.startsWith(config.engine.directory + path.sep) && filename !== __filename) track(filename);
            }
            transmit(connection, { type: 'done', code: failed ? 1 : 0 });
            if (failed) {
                closing = true;
                removeOwnRecord(config.directory, config.nonce);
                connection.end(shutdown);
            }
        } catch (error) {
            output('stderr', `HTTP execution failed: ${error.stack || error}\n`);
            closing = true;
            removeOwnRecord(config.directory, config.nonce);
            connection.end(JSON.stringify({ type: 'done', code: 1 }) + '\n', shutdown);
        } finally {
            active = undefined;
            lastUsed = Date.now();
        }
    }
    const listener = net.createServer(connection => {
        let accepted = false;
        const timer = setTimeout(() => { if (!accepted) connection.destroy(); }, 5000);
        connection.on('error', () => connection.destroy());
        connection.on('close', () => {
            clearTimeout(timer);
            if (active?.socket === connection) {
                active.canceled = true;
                for (const cancel of active.cancellations) { try { cancel(); } catch (error) { output('stderr', `${error}\n`); } }
                for (const prompt of active.prompts.values()) prompt.reject(new Error('Request canceled'));
                shutdown();
            }
        });
        frames(connection, message => {
            if (accepted) {
                const pending = active?.socket === connection && active.prompts.get(message.answer);
                if (!pending) throw new Error('Unexpected executor message');
                active.prompts.delete(message.answer);
                if (message.error) pending.reject(new Error(message.error)); else pending.resolve(message.value);
                return;
            }
            if (typeof message.token !== 'string' || message.token.length !== token.length || !crypto.timingSafeEqual(Buffer.from(message.token), Buffer.from(token))) throw new Error('Unauthorized executor connection');
            accepted = true;
            clearTimeout(timer);
            if (message.operation === 'reset') {
                // Reset must also work while a request/stream is waiting indefinitely.
                closing = true;
                removeOwnRecord(config.directory, config.nonce);
                connection.end(JSON.stringify({ type: 'done', code: 0 }) + '\n', shutdown);
                return;
            }
            queue = queue.then(() => execute(message, connection)).catch(error => { output('stderr', `${error}\n`); shutdown(); });
        });
    });
    listener.on('error', shutdown);
    await new Promise(resolve => listener.listen(0, '127.0.0.1', resolve));
    if (readRecord(config.directory)?.nonce !== config.nonce) { listener.close(); return; }
    writeRecord(config.directory, { pid: process.pid, port: listener.address().port, token, nonce: config.nonce, root: config.root });
    setInterval(() => {
        if (!alive(owner) || !fs.existsSync(sessionDirectory) || (!active && Date.now() - lastUsed > 30 * 60 * 1000)) shutdown();
    }, 2000).unref();
    process.on('SIGTERM', shutdown);
    process.on('SIGINT', shutdown);
    process.on('exit', () => removeOwnRecord(config.directory, config.nonce));
}

if (process.argv[2] === '--server') {
    server(JSON.parse(process.argv[3])).catch(error => { console.error(error); process.exitCode = 1; });
} else {
    client().then(() => {
        if (machine) { emit({ type: 'done', code: process.exitCode || 0 }); process.stdin.destroy(); }
    }).catch(error => {
        if (machine) {
            emit({ type: 'error', message: error.message });
            emit({ type: 'done', code: process.exitCode || 1 });
            process.stdin.destroy();
        } else console.error(error.message);
        process.exitCode = process.exitCode || 1;
    });
}
