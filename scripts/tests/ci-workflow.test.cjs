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
    'Copilot Metrics PowerShell Tests', 'Test CI workflow bindings']) {
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
