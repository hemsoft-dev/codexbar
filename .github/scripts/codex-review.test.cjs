'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { codexVerdict, fallbackReceipt, run } = require('./codex-review.cjs');
const head = 'a'.repeat(40);
const clean = { commit_sha: head, status: 'completed', has_any_findings: false, has_unresolved_findings: false };
const summary = (result, user = { login: 'chatgpt-codex-connector[bot]', type: 'Bot' }) => ({
  user, body: `<!-- codex-review-state:v1 ${JSON.stringify({ code_review: result })} -->`, updated_at: '2026-10-22', html_url: 'https://github.com/review'
});

test('only a completed clean bot verdict for the exact head passes', () => {
  assert.equal(codexVerdict([summary(clean)], head).state, 'success');
  for (const result of [ { ...clean, commit_sha: 'b'.repeat(40) }, { ...clean, status: 'running' },
    { ...clean, has_any_findings: true }, { ...clean, has_unresolved_findings: true },
    { ...clean, has_any_findings: null }, { ...clean, has_unresolved_findings: undefined } ]) {
    assert.notEqual(codexVerdict([summary(result)], head).state, 'success');
  }
  assert.equal(codexVerdict([], head).state, 'pending');
  assert.equal(codexVerdict([summary(clean, { login: 'someone', type: 'User' })], head).state, 'pending');
  assert.equal(codexVerdict([summary(clean, { login: 'chatgpt-codex-connector[bot]', type: 'User' })], head).state, 'pending');
  assert.equal(codexVerdict([{ ...summary(clean), body: '<!-- codex-review-state:v1 {broken} -->' }], head).state, 'pending');
});

test('a later running or failed result replaces an earlier clean result', () => {
  const later = { ...summary({ ...clean, status: 'running' }), updated_at: '2026-10-23' };
  assert.equal(codexVerdict([summary(clean), later], head).state, 'pending');
});

const receipt = { head, clean: true, provider: 'claude', model: 'claude-opus-5-5', codex: 'hung' };
const fallback = { user: { login: 'HemSoft', type: 'User' }, commit_id: head, state: 'COMMENTED',
  body: `<!-- codexbar-review-fallback:v1 ${JSON.stringify(receipt)} -->`, html_url: 'https://github.com/fallback' };
test('fallback requires the named maintainer, exact head, model and explicit clean receipt', () => {
  assert.equal(fallbackReceipt(fallback, head), true);
  for (const change of [{ commit_id: 'b'.repeat(40) }, { state: 'CHANGES_REQUESTED' },
    { user: { login: 'someone', type: 'User' } }, { body: 'clean' },
    { body: `<!-- codexbar-review-fallback:v1 ${JSON.stringify({ ...receipt, clean: false })} -->` }]) {
    assert.equal(fallbackReceipt({ ...fallback, ...change }, head), false);
  }
});

function client({ moved = false, reviews = [], comments = [], permission = 'admin' } = {}) {
  const statuses = []; let reads = 0;
  const github = { paginate: async method => method(), rest: {
    pulls: { get: async () => ({ data: { state: 'open', base: { ref: 'main' }, head: { sha: moved && reads++ ? 'b'.repeat(40) : head } } }), listReviews: async () => reviews },
    issues: { listComments: async () => comments },
    repos: { createCommitStatus: async value => statuses.push(value), getCollaboratorPermissionLevel: async () => ({ data: { permission } }) }
  } };
  return { github, statuses, context: { repo: { owner: 'hemsoft-dev', repo: 'codexbar' }, payload: { pull_request: { number: 1 } } }, core: { info() {} } };
}

test('a concurrent head change cannot publish a stale decision', async () => {
  const fixture = client({ moved: true }); await run(fixture); assert.equal(fixture.statuses.length, 0);
});
test('missing evidence publishes pending on the exact head', async () => {
  const fixture = client(); await run(fixture);
  assert.equal(fixture.statuses[0].sha, head); assert.equal(fixture.statuses[0].state, 'pending');
});
test('fallback also verifies live maintainer permission', async () => {
  const allowed = client({ reviews: [fallback] }); await run(allowed); assert.equal(allowed.statuses[0].state, 'success');
  const refused = client({ reviews: [fallback], permission: 'write' }); await run(refused); assert.equal(refused.statuses[0].state, 'pending');
});

test('fallback cannot hide Codex findings or a later non-clean maintainer review', async () => {
  const findings = client({ reviews: [fallback], comments: [summary({ ...clean, has_any_findings: true })] });
  await run(findings); assert.equal(findings.statuses[0].state, 'failure');
  const later = client({ reviews: [fallback, { ...fallback, body: 'P2: fix this' }] });
  await run(later); assert.equal(later.statuses[0].state, 'pending');
});
