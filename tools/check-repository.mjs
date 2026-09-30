import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = new Set(process.argv.slice(2));
const identity = 'ydadev';
const email = '300869099+ydadev@users.noreply.github.com';
const git = (...argv) => execFileSync('git', argv, { cwd: root, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
const failures = [];
const fail = message => failures.push(message);
const validDate = value => /^\d{4}-\d{2}-\d{2}$/.test(value) &&
  !Number.isNaN(Date.parse(value)) && new Date(value).toISOString().slice(0, 10) === value;
const validMessage = value => /^(?:\d{4}-\d{2}-\d{2})\n?$/.test(value) && validDate(value.replace(/\n$/, ''));

if (args.has('--commit-message')) {
  const file = process.argv[process.argv.indexOf('--commit-message') + 1];
  const message = readFileSync(file, 'utf8').replace(/\r\n/g, '\n');
  if (!validMessage(message))
    fail('Commit message must contain only a valid UTC date YYYY-MM-DD.');
} else {
  const staged = args.has('--staged');
  const files = git('ls-files', '-z').split('\0').filter(Boolean);
  const migrationVersions = new Map();
  for (const file of files) {
    const migration = file.match(/^(crates\/[^/]+\/migrations\/)(\d+)_.*\.sql$/);
    if (migration) {
      const version = `${migration[1]}${Number(migration[2])}`;
      if (migrationVersions.has(version))
        fail(`Duplicate migration version: ${migrationVersions.get(version)} and ${file}`);
      else migrationVersions.set(version, file);
    }
    if (/^(?:\.local|node_modules|target|dist)\//.test(file) || /(?:^|\/)secrets\//.test(file) ||
        /(?:^|\/)\.env(?:\..*)?$/.test(file) && !file.endsWith('.env.example') ||
        /\.(?:pem|key|p12|pfx|dump|log)$/.test(file)) fail(`Forbidden tracked path: ${file}`);
    const body = staged ? git('show', `:${file}`) : readFileSync(path.join(root, file), 'utf8');
    // Guards for accidental publication; this is not a substitute for diff review.
    if (/[A-Z]:[\\/]Users[\\/][^\s/\\]+/i.test(body) || /\/home\/[^\s/]+\//.test(body))
      fail(`Private home path: ${file}`);
    if (new RegExp('-----BEGIN ' + '(?:RSA |OPENSSH |EC )?PRIVATE KEY-----').test(body))
      fail(`Private key: ${file}`);
    if (/\b(?:10\.\d{1,3}\.\d{1,3}\.\d{1,3}|192\.168\.\d{1,3}\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3})\b/.test(body))
      fail(`Private address: ${file}`);
    if (file.endsWith('.md')) {
      for (const match of body.matchAll(/\[[^\]]*\]\(([^\s)]+)(?:\s+"[^"]*")?\)/g)) {
        const href = match[1];
        if (/^(?:https?:|mailto:|#)/.test(href)) continue;
        const target = path.resolve(root, path.dirname(file), decodeURIComponent(href.split('#')[0]));
        if (!existsSync(target)) fail(`Missing link in ${file}: ${href}`);
      }
    }
  }
  if (args.has('--identity')) {
    for (const kind of ['AUTHOR', 'COMMITTER']) {
      const value = git('var', `GIT_${kind}_IDENT`);
      if (!value.startsWith(`${identity} <${email}> `)) fail(`Invalid ${kind.toLowerCase()} identity.`);
    }
  }
  if (args.has('--history')) {
    const commits = git('rev-list', '--all').trim().split('\n').filter(Boolean);
    for (const commit of commits) {
      const [author, authorEmail, committer, committerEmail] =
        git('show', '-s', '--format=%an%x00%ae%x00%cn%x00%ce', commit).trimEnd().split('\0');
      const object = git('cat-file', 'commit', commit);
      const message = object.slice(object.indexOf('\n\n') + 2);
      if (author !== identity || committer !== identity || authorEmail !== email || committerEmail !== email)
        fail(`Invalid identity in commit ${commit.slice(0, 12)}.`);
      if (!validMessage(message)) fail(`Invalid commit message in ${commit.slice(0, 12)}.`);
    }
  }
}
if (failures.length) {
  for (const failure of failures) console.error(failure);
  process.exit(1);
}
console.log('Repository policy: PASS');
