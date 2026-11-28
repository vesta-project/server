import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

function inside(root, file) {
  const relative = path.relative(root, file);
  if (!relative || relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
    throw new Error(`Path is outside ${root}: ${file}`);
  }
  return relative.split(path.sep).join('/');
}

// Cargo can report the same artifact more than once. Keep both test harnesses
// and ordinary executable targets (integration tests use CARGO_BIN_EXE paths).
export function cargoExecutables(messages, workspace, target) {
  const executables = new Map();
  for (const line of messages.split(/\r?\n/).filter(Boolean)) {
    const message = JSON.parse(line);
    if (message.reason !== 'compiler-artifact' || !message.executable) continue;
    if (!message.profile.test && message.target?.kind.includes('test')) {
      throw new Error(`Test target does not use libtest: ${message.target.name}`);
    }
    const executable = inside(workspace, message.executable);
    inside(target, message.executable);
    if (message.profile.test) {
      const cwd = path.dirname(message.manifest_path);
      executables.set(executable, { executable, cwd: cwd === workspace ? '.' : inside(workspace, cwd) });
    } else if (!executables.has(executable)) {
      executables.set(executable, { executable });
    }
  }
  return [...executables.values()].sort((a, b) => a.executable.localeCompare(b.executable));
}

export function parseLibtestList(output) {
  const names = [];
  for (const line of output.split(/\r?\n/).filter(Boolean)) {
    const match = line.match(/^([^\s-][^\r\n]*): (test|benchmark)$/);
    if (!match) throw new Error(`Unexpected libtest listing: ${line}`);
    names.push(match[1]);
  }
  if (new Set(names).size !== names.length) throw new Error('Duplicate libtest name');
  return names;
}

export function partitionSuites(suites, count, estimate = () => 1) {
  if (!Number.isInteger(count) || count < 1) throw new Error('Invalid shard count');
  const shards = Array.from({length: count}, () => ({seconds: 0, suites: new Map()}));
  const cases = suites.flatMap((suite, index) => (suite.names.length ? suite.names : [null]).map(name => ({
    index, name, seconds: name === null ? 0.01 : estimate(suite, name),
  })));
  // Schedule the most expensive cases first on the least loaded runner. The
  // estimates only affect ordering; every listed case still gets one shard.
  cases.sort((a, b) => b.seconds - a.seconds);
  for (const {index, name, seconds} of cases) {
    const shard = shards.reduce((least, next) => next.seconds < least.seconds ? next : least);
    shard.seconds += seconds;
    if (!shard.suites.has(index)) shard.suites.set(index, []);
    if (name !== null) shard.suites.get(index).push(name);
  }
  return shards.map(shard => [...shard.suites.entries()].sort(([a], [b]) => a - b).map(([index, names]) => ({
    executable: suites[index].executable, cwd: suites[index].cwd, names,
  })));
}

export function testArguments(names) {
  // Stay well within the Windows CreateProcess command-line limit, including
  // quoting overhead and space for the absolute executable path.
  const prefix = ['--test-threads=1', '--exact'];
  const batches = [[]];
  let length = 0;
  for (const name of names) {
    const size = name.length * 2 + 3;
    if (size > 16000) throw new Error('Test name exceeds command-line budget');
    if (length + size > 16000) {
      batches.push([]);
      length = 0;
    }
    batches.at(-1).push(name);
    length += size;
  }
  return batches.map(batch => [...prefix, ...batch]);
}

function gitHead() {
  return execFileSync('git', ['rev-parse', 'HEAD'], {encoding: 'utf8'}).trim();
}

function prepare(messagesFile, target, count) {
  const workspace = process.cwd();
  const archive = path.join(target, 'ci-test-shards.json');
  const files = cargoExecutables(fs.readFileSync(messagesFile, 'utf8'), workspace, target);
  const suites = files.filter(file => file.cwd).map(file => ({
    ...file,
    names: parseLibtestList(execFileSync(path.resolve(file.executable), ['--list', '--format=terse'], {
      cwd: file.cwd, encoding: 'utf8', maxBuffer: 8 * 1024 * 1024,
    })),
  }));
  if (suites.length === 0) throw new Error('Cargo reported no test executables');
  const timings = JSON.parse(fs.readFileSync(new URL('./fixtures/ci-rust-test-timings.json', import.meta.url), 'utf8'));
  const platform = timings.platforms.indexOf(process.platform);
  const estimate = (suite, name) => {
    const measured = timings.seconds[name]?.[platform];
    if (typeof measured === 'number' && measured > 0) return measured;
    const executable = path.posix.basename(suite.executable);
    if (executable.startsWith('https-')) return 2.2;
    if (executable.startsWith('vesta_transport-')) return 1.1;
    return 0.5;
  };
  const shards = partitionSuites(suites, count, estimate);
  fs.writeFileSync(archive, JSON.stringify({commit: gitHead(), workspace, shards}));
  for (const file of files) process.stdout.write(`${file.executable}\n`);
  process.stdout.write(`${inside(workspace, archive)}\n`);
  console.error(`Planned ${suites.reduce((sum, suite) => sum + suite.names.length, 0)} test cases in ${suites.length} executables across ${count} shards`);
  const estimates = shards.map(shard => Math.round(shard.reduce((total, suite) => total
    + suite.names.reduce((seconds, name) => seconds + estimate(suite, name), 0), 0)));
  console.error(`Estimated seconds per shard: ${estimates.join(', ')}`);
}

function run(manifest, index) {
  const {commit, workspace, shards} = JSON.parse(fs.readFileSync(manifest, 'utf8'));
  if (commit !== gitHead() || workspace !== process.cwd()) {
    throw new Error('Test archive source or checkout path differs from the build runner');
  }
  if (!Number.isInteger(index) || !shards[index]) throw new Error('Invalid shard index');
  let success = true;
  for (const suite of shards[index]) {
    console.log(`=== ${suite.executable} (${suite.names.length} selected cases) ===`);
    for (const args of testArguments(suite.names)) {
      const result = spawnSync(path.resolve(suite.executable), args, {cwd: suite.cwd, stdio: 'inherit'});
      if (result.error) throw result.error;
      success &&= result.status === 0;
    }
  }
  if (!success) process.exitCode = 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [, , command, input, target, count] = process.argv;
  if (command === 'prepare') prepare(input, path.resolve(target), Number(count));
  else if (command === 'run') run(input, Number(target));
  else throw new Error('Expected prepare or run');
}
