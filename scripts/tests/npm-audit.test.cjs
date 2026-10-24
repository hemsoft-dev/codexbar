// Exercise npm's real advisory producer and the same package script used by CI.
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join, resolve } = require('node:path');
const { test } = require('node:test');

const root = resolve(__dirname, '../..');
const npm = process.env.npm_execpath;
assert.ok(npm, 'Run through npm run test:npm-audit so the same npm CLI is used on every OS');

function audit(directory, env = {}) {
  const result = spawnSync(process.execPath, [npm, 'run', '--silent', 'audit:npm', '--',
    '--prefix', directory, '--package-lock-only', '--json'], {
    cwd: root, encoding: 'utf8', timeout: 60_000,
    env: { ...process.env, ...env },
  });
  assert.ifError(result.error);
  assert.equal(result.signal, null, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(report.auditReportVersion, 2, JSON.stringify(report));
  assert.equal(report.error, undefined, JSON.stringify(report));
  return { status: result.status, report };
}

test('the current development dependency lock passes the all-severity policy', () => {
  const { status, report } = audit(root);
  assert.equal(status, 0, JSON.stringify(report));
  assert.equal(report.metadata.vulnerabilities.total, 0);
});

test('an actual vulnerable development lock fails even with conflicting npm defaults', () => {
  const directory = mkdtempSync(join(tmpdir(), 'codexbar-npm-audit-'));
  try {
    const fixture = join(root, 'scripts/fixtures/npm-vulnerable-markdown');
    for (const file of ['package.json', 'package-lock.json']) {
      cpSync(join(fixture, file), join(directory, file));
    }
    const before = readFileSync(join(directory, 'package-lock.json'));
    const { status, report } = audit(directory, {
      npm_config_audit_level: 'critical', npm_config_omit: 'dev',
    });
    assert.equal(status, 1, JSON.stringify(report));
    const affected = report.vulnerabilities.katex;
    assert.equal(affected.severity, 'low');
    assert.equal(affected.isDirect, true);
    assert.ok(affected.via.some(advisory => typeof advisory === 'object'
      && advisory.url === 'https://github.com/advisories/GHSA-238p-pmpm-9mq7'));
    assert.ok(report.metadata.vulnerabilities.total > 0);
    assert.deepEqual(readFileSync(join(directory, 'package-lock.json')), before,
      'audit must never fix or rewrite the vulnerable fixture');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

// Overrides cross upstream's minor-version ranges. Exercise the parser APIs the CLI actually uses.
test('the upgraded CLI reads JSON, YAML and TOML configs and still rejects Markdown errors', () => {
  const directory = mkdtempSync(join(tmpdir(), 'codexbar-markdown-'));
  try {
    const file = join(directory, 'sample.md');
    writeFileSync(file, '# Math fixture\n\nInline $x + y$ and display math.\n\n$$\nx^2 + y^2\n$$\n');
    const cli = join(root, 'node_modules/markdownlint-cli/markdownlint.js');
    for (const [extension, content] of [
      ['json', '{"MD013":false}'], ['yaml', 'MD013: false\n'], ['toml', 'MD013 = false\n'],
    ]) {
      const config = join(directory, `config.${extension}`);
      writeFileSync(config, content);
      const result = spawnSync(process.execPath, [cli, '--config', config, '--', file], {
        cwd: directory, encoding: 'utf8', timeout: 30_000,
      });
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stdout + result.stderr);
    }
    writeFileSync(file, '# Heading\n\n### Skipped level\n');
    const invalid = spawnSync(process.execPath, [cli, '--', file], {
      cwd: directory, encoding: 'utf8', timeout: 30_000,
    });
    assert.ifError(invalid.error);
    assert.equal(invalid.status, 1);
    assert.match(invalid.stderr, /MD001/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
