const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const yaml = require('js-yaml');

const workflow = yaml.load(fs.readFileSync(path.join(__dirname, '../../.github/workflows/ci.yml'), 'utf8'));
const build = workflow.jobs.build;
const retiredIds = ['dotnet', 'restore', 'build', 'format', 'test', 'tools', 'mutations', 'security'];

test('CI triggers and Rust/security check names stay unconditional', () => {
  assert.deepEqual(Object.keys(workflow.on).sort(), ['pull_request', 'push']);
  for (const trigger of Object.values(workflow.on)) {
    assert.deepEqual(trigger.branches, ['main']);
    assert.equal(trigger.paths, undefined);
    assert.equal(trigger['paths-ignore'], undefined);
  }
  assert.deepEqual(Object.keys(workflow.jobs).sort(), ['build', 'rust', 'rust-metrics', 'rust-mutations', 'security-scan']);
  for (const job of Object.values(workflow.jobs)) {
    assert.equal(job.if, undefined);
    assert.equal(job.needs, undefined);
  }
  assert.deepEqual(workflow.jobs.rust.strategy.matrix.os, ['windows-latest', 'macos-latest', 'ubuntu-latest']);
});

test('only retired steps and their report use input-selection conditions', () => {
  const gated = build.steps.filter(step => step.if && step.name !== 'Upload CI input selection'
    && step.name !== 'Verify current-job retired qualification'
    && step.name !== 'Upload Mutation Report');
  // Existing npm audit upload uses always(), independently of the retired selector.
  const selected = gated.filter(step => step.if !== 'always()');
  assert.deepEqual(selected.map(step => step.id), retiredIds);
  for (const step of selected) {
    assert.equal(step.if, "steps.inputs.outputs.retired == 'true'");
    assert.equal(step['continue-on-error'], undefined);
  }
  assert.equal(build.steps.find(step => step.name === 'Upload Mutation Report').if,
    "always() && steps.inputs.outputs.retired == 'true'");
});

test('shared checks remain ordinary required steps', () => {
  for (const name of ['Setup Node', 'Install markdownlint-cli',
    'Audit all npm dependencies, including development tools',
    'Prove npm advisory policy rejects vulnerable locks', 'Markdown Lint',
    'Copilot Metrics PowerShell Tests', 'Copilot Metrics Windows Launch Tests', 'Test CI workflow bindings']) {
    const step = build.steps.find(candidate => candidate.name === name);
    assert.ok(step, name);
    assert.equal(step.if, undefined, name);
    assert.equal(step['continue-on-error'], undefined, name);
  }
});

test('selection uses complete history and final verification sees only this job', () => {
  assert.equal(build.steps[0].with['fetch-depth'], 0);
  const selector = build.steps.find(step => step.id === 'inputs');
  assert.equal(selector.run, 'python scripts/ci-inputs.py select');
  assert.equal(selector.if, undefined);
  const final = build.steps.find(step => step.name === 'Verify current-job retired qualification');
  assert.equal(final.if, 'always()');
  assert.equal(final.env.CI_STEPS_JSON, '${{ toJSON(steps) }}');
  assert.equal(final.run, 'python scripts/ci-inputs.py verify');
  assert.equal(final['continue-on-error'], undefined);
  for (const id of retiredIds) {
    assert.ok(build.steps.indexOf(final) > build.steps.findIndex(step => step.id === id));
  }
});

// Clippy and the tests unify codexbar-app's dev-dependency features (gpui-kit `test-support`) into the binary, so
// only a check of the binary alone sees the build users install (#297). It runs on every platform, unconditionally.
test('every platform checks the production binary without dev-dependency features', () => {
  const steps = workflow.jobs.rust.steps;
  const production = steps.filter(step => typeof step.run === 'string'
    && /^cargo (check|build)\b/.test(step.run.trim()));
  assert.equal(production.length, 1);
  const [step] = production;
  assert.equal(step.name, 'Check production binary');
  assert.equal(step.run, 'cargo check -p codexbar-app --bin codexbar --locked');
  assert.equal(step.if, undefined);
  assert.equal(step['continue-on-error'], undefined);
  assert.equal(step.env, undefined);
  const names = steps.map(candidate => candidate.name);
  assert.ok(names.indexOf('Install Rust toolchain') < steps.indexOf(step));
  assert.ok(steps.indexOf(step) < names.indexOf('Test'));
});

// Every action is pinned to a reviewed commit, with the release it came from in a trailing comment (#301).
// A movable tag such as `@v4` could change what runs after review, so a new reference must also be a SHA.
test('every action reference in CI and Package is a full commit SHA with its version', () => {
  for (const name of ['ci.yml', 'package.yml']) {
    const text = fs.readFileSync(path.join(__dirname, '../../.github/workflows', name), 'utf8');
    const references = text.split(/\r?\n/).filter(line => /^\s*(- )?uses:/.test(line));
    assert.ok(references.length > 0, name);
    for (const line of references) {
      assert.match(line, /^\s*(- )?uses: [\w.-]+\/[\w./-]+@[0-9a-f]{40} # v\d+\.\d+\.\d+$/, `${name}: ${line.trim()}`);
    }
  }
});
