const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');

const script = path.resolve(__dirname, '../publish-npm.sh');

function publish(t, response, lookupStatus = 0, publishStatus = 0) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'radio-publish-test-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const packageDir = path.join(dir, 'package with spaces');
  fs.mkdirSync(packageDir);
  fs.writeFileSync(path.join(packageDir, 'package.json'), JSON.stringify({
    name: '@hmgle/radio-cli', version: '1.2.3',
  }));
  const calls = path.join(dir, 'calls');
  fs.writeFileSync(path.join(dir, 'npm'), `#!/usr/bin/env node
const fs = require('node:fs');
fs.appendFileSync(process.env.CALLS, JSON.stringify(process.argv.slice(2)) + '\\n');
if (process.argv[2] === 'view') {
  process.stdout.write(process.env.RESPONSE);
  process.exit(Number(process.env.LOOKUP_STATUS));
}
if (process.argv[2] === 'publish') process.exit(Number(process.env.PUBLISH_STATUS));
process.exit(99);
`, { mode: 0o755 });
  const result = spawnSync('bash', [script, packageDir], {
    encoding: 'utf8',
    env: {
      ...process.env, PATH: `${dir}${path.delimiter}${process.env.PATH}`,
      CALLS: calls, RESPONSE: response,
      LOOKUP_STATUS: String(lookupStatus), PUBLISH_STATUS: String(publishStatus),
    },
  });
  assert.ifError(result.error);
  const commands = fs.readFileSync(calls, 'utf8').trim().split('\n').map(JSON.parse);
  assert.deepEqual(commands[0], [
    'view', '@hmgle/radio-cli@1.2.3', 'version', '--json', '--prefer-online',
  ]);
  return { result, commands, packageDir };
}

// Preserve both response shapes observed in real exact-version lookups.
for (const [format, response] of [
  ['array', '[\n  "1.2.3"\n]\n'], // npm 12.2.1
  ['string', '"1.2.3"\n'], // npm 11.13.0
]) {
  test(`an existing exact version is skipped with the ${format} response`, (t) => {
    const { result, commands } = publish(t, response);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Already published @hmgle\/radio-cli@1\.2\.3/);
    assert.equal(commands.length, 1);
  });
}

test('a missing version is published with provenance', (t) => {
  const { result, commands, packageDir } = publish(t, '{"error":{"code":"E404"}}', 1);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(commands[1], ['publish', packageDir, '--provenance']);
  assert.equal(commands.length, 2);
});

test('registry errors never trigger an upload', (t) => {
  for (const code of ['E401', 'E403', 'E500', 'ENOTFOUND']) {
    const { result, commands } = publish(t, JSON.stringify({ error: { code } }), 1);
    assert.notEqual(result.status, 0);
    assert.equal(commands.length, 1);
  }
});

test('unexpected lookup responses fail closed', (t) => {
  for (const response of [
    '', 'not JSON', '[]', 'null', '"1.2.4"', '["1.2.4"]',
    '["1.2.3", "1.2.4"]', '["1.2.3", "1.2.3"]',
    '[["1.2.3"]]', '{"version":"1.2.3"}',
  ]) {
    for (const status of [0, 1]) {
      const { result, commands } = publish(t, response, status);
      assert.notEqual(result.status, 0);
      assert.equal(commands.length, 1);
      if (status === 0) {
        assert.match(result.stderr, /Unexpected registry response for @hmgle\/radio-cli@1\.2\.3:/);
      }
    }
  }
});

test('a failed upload remains a failure', (t) => {
  const { result, commands } = publish(t, '{"error":{"code":"E404"}}', 1, 42);
  assert.equal(result.status, 42);
  assert.equal(commands.length, 2);
});
