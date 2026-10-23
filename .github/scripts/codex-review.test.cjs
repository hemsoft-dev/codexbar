'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { codexVerdict, fallbackReceipt, eligibleEvent, triggerIncluded, workflowRunContext, run } = require('./codex-review.cjs');
const head = 'a'.repeat(40);
const clean = { commit_sha: head, status: 'completed', has_any_findings: false, has_unresolved_findings: false };
const summary = (result, user = { login: 'chatgpt-codex-connector[bot]', type: 'Bot' }) => ({
  id: 42, node_id: 'comment42', user, body: `<!-- codex-review-state:v1 ${JSON.stringify({ code_review: result })} -->`, updated_at: '2026-10-22', html_url: 'https://github.com/review'
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
  assert.equal(codexVerdict([{ ...summary(clean), body: '<!-- codex-review-state:v1 {broken} -->' }], head).state, 'failure');
});

test('a later running or failed result replaces an earlier clean result', () => {
  const later = { ...summary({ ...clean, status: 'running' }), updated_at: '2026-10-23' };
  assert.equal(codexVerdict([summary(clean), later], head).state, 'pending');
});

const receipt = { head, clean: true, provider: 'claude', model: 'claude-opus-5-5', codex: 'hung' };
const fallback = { user: { id: 8227352, login: 'HemSoft', type: 'User' }, commit_id: head, state: 'COMMENTED',
  body: `<!-- codexbar-review-fallback:v1 ${JSON.stringify(receipt)} -->`, html_url: 'https://github.com/fallback' };
test('fallback requires the named maintainer, exact head, model and explicit clean receipt', () => {
  assert.equal(fallbackReceipt(fallback, head), true);
  for (const change of [{ commit_id: 'b'.repeat(40) }, { state: 'CHANGES_REQUESTED' },
    { user: { login: 'someone', type: 'User' } }, { body: 'clean' },
    { body: `<!-- codexbar-review-fallback:v1 ${JSON.stringify({ ...receipt, clean: false })} -->` }]) {
    assert.equal(fallbackReceipt({ ...fallback, ...change }, head), false);
  }
});

const merge = 'c'.repeat(40);
function client({ moved = false, mergeMoved = false, reviews = [], comments = [],
  freshComments = comments, freshReviews = reviews, permission = 'admin', priorStatuses = [], statusError, payload = { pull_request: { number: 1 } } } = {}) {
  const statuses = []; let reads = 0; let commentReads = 0; let reviewReads = 0;
  const github = { graphql: async (_query, { id }) => {
    const comment = [...freshComments, ...comments].find(item => item.node_id === id);
    return { node: comment ? { body: comment.body, updatedAt: comment.updated_at,
      author: { login: 'chatgpt-codex-connector', __typename: 'Bot' },
      lastEditedAt: comment.editedAt ?? null, editor: comment.editor ?? null } : null };
  }, paginate: async method => method(), rest: {
    pulls: { get: async () => {
      const later = reads++ > 0;
      return { data: { state: 'open', base: { ref: 'main' }, head: { sha: moved && later ? 'b'.repeat(40) : head },
        merge_commit_sha: mergeMoved && later ? 'd'.repeat(40) : merge } };
    }, listReviews: async () => reviewReads++ ? freshReviews : reviews },
    issues: { listComments: async () => commentReads++ ? freshComments : comments },
    repos: { listCommitStatusesForRef: async () => priorStatuses, createCommitStatus: async value => {
      if (statusError?.(value)) throw new Error('status write failed');
      statuses.push(value);
    }, getCollaboratorPermissionLevel: async () => ({ data: { permission } }) }
  } };
  return { github, statuses, context: { repo: { owner: 'hemsoft-dev', repo: 'codexbar' }, payload }, core: { info() {} } };
}
const result = fixture => fixture.statuses.at(-1).state;

test('a concurrent head or merge change cannot publish success', async () => {
  for (const change of [{ moved: true }, { mergeMoved: true }]) {
    const fixture = client({ ...change, comments: [summary(clean)] }); await run(fixture);
    const gates = fixture.statuses.filter(item => item.context === 'codex-review');
    assert.equal(gates.length, 2);
    assert.ok(gates.every(status => status.state === 'pending'));
  }
});
test('missing evidence publishes pending on both head and test merge', async () => {
  const fixture = client(); await run(fixture);
  assert.deepEqual(fixture.statuses.slice(-2).map(status => status.sha), [merge, head]);
  assert.equal(result(fixture), 'pending');
});
test('clean review publishes success on head and test merge', async () => {
  const fixture = client({ comments: [summary(clean)] }); await run(fixture);
  assert.deepEqual(fixture.statuses.slice(-2).map(status => [status.sha, status.state]), [[merge, 'success'], [head, 'success']]);
});
test('fallback also verifies live maintainer permission', async () => {
  const allowed = client({ reviews: [fallback] }); await run(allowed); assert.equal(result(allowed), 'success');
  const refused = client({ reviews: [fallback], permission: 'write' }); await run(refused); assert.equal(result(refused), 'pending');
});
test('fallback cannot hide Codex findings or a later non-clean maintainer review', async () => {
  const findings = client({ reviews: [fallback], comments: [summary({ ...clean, has_any_findings: true })] });
  await run(findings); assert.equal(result(findings), 'failure');
  const later = client({ reviews: [fallback, { ...fallback, body: 'P2: fix this' }] });
  await run(later); assert.equal(result(later), 'pending');
});
test('the maintainer may rename without trusting an unrelated account with the old login', () => {
  assert.equal(fallbackReceipt({ ...fallback, user: { ...fallback.user, login: 'renamed' } }, head), true);
  assert.equal(fallbackReceipt({ ...fallback, user: { ...fallback.user, id: 1 } }, head), false);
});
test('summary or fallback mutation or deletion before publication leaves pending', async () => {
  for (const freshComments of [[], [summary({ ...clean, status: 'running' })], [summary({ ...clean, has_any_findings: true })]]) {
    const fixture = client({ comments: [summary(clean)], freshComments }); await run(fixture);
    assert.equal(result(fixture), 'pending');
  }
  for (const freshReviews of [[], [{ ...fallback, body: 'findings' }]]) {
    const fixture = client({ reviews: [fallback], freshReviews }); await run(fixture);
    assert.equal(result(fixture), 'pending');
  }
});
test('a stale API snapshot cannot ignore the triggering edit or deletion', async () => {
  const old = { ...summary(clean), id: 42 };
  const edited = { ...old, body: 'running', updated_at: '2026-10-23' };
  for (const [action, comment] of [['edited', edited], ['deleted', old]]) {
    const fixture = client({ comments: [old], payload: { issue: { number: 1 }, action, comment } });
    await run(fixture); assert.equal(result(fixture), 'pending');
  }
  assert.equal(triggerIncluded({ payload: { action: 'deleted', comment: old } }, [], []), true);
});

test('a newer malformed summary cannot resurrect an older clean result', () => {
  for (const body of ['<!-- codex-review-state:v1 {broken} -->', '<!-- codex-review-state:v1 {} -->',
    '<!-- codex-review-state:v1 {"code_review":null} -->', '<!-- codex-review-state:v1 truncated']) {
    assert.equal(codexVerdict([summary(clean), { ...summary(clean), body, updated_at: '2026-10-24' }], head).state, 'failure');
  }
});
test('unrelated comments and reviews cannot revoke a clean gate', async () => {
  for (const eventName of ['issue_comment', 'pull_request_review']) {
    const fixture = client({ comments: [summary(clean)], payload: { issue: { number: 1, pull_request: {} },
      comment: { user: { login: 'guest', type: 'User' }, body: 'ordinary conversation' },
      review: { user: { id: 1 } } } });
    fixture.context.eventName = eventName;
    await run(fixture); assert.equal(fixture.statuses.length, 0);
  }
  assert.equal(eligibleEvent({ eventName: 'issue_comment', payload: { issue: { pull_request: {} }, comment: summary(clean) } }), true);
});

const watermark = (id, at, state = 'success') => ({ context: 'codex-review-evidence', state,
  creator: { login: 'github-actions[bot]', type: 'Bot' }, description: JSON.stringify({ id, at }) });
test('a collaborator edit is not trusted on either an event or manual refresh', async () => {
  for (const eventName of ['issue_comment', 'workflow_dispatch']) {
    const forged = { ...summary(clean), editedAt: '2026-10-22', editor: { login: 'writer', __typename: 'User' } };
    const fixture = client({ comments: [forged], payload: { issue: { number: 1, pull_request: {} }, comment: forged, action: 'edited' } });
    fixture.context.eventName = eventName;
    await run(fixture); assert.equal(result(fixture), 'failure');
  }
  const restored = { ...summary(clean), editedAt: '2026-10-22', editor: { login: 'chatgpt-codex-connector', __typename: 'Bot' } };
  const fixture = client({ comments: [restored] }); await run(fixture); assert.equal(result(fixture), 'success');
});
test('deletion latches failure instead of reviving an older clean summary on refresh', async () => {
  const deleted = { ...summary({ ...clean, has_any_findings: true }), id: 43, updated_at: '2026-10-23' };
  const fixture = client({ comments: [summary(clean)], payload: { issue: { number: 1, pull_request: {} }, comment: deleted, action: 'deleted' } });
  fixture.context.eventName = 'issue_comment';
  await run(fixture); assert.equal(result(fixture), 'failure');
  const latch = fixture.statuses.find(item => item.context === 'codex-review-evidence');
  const refresh = client({ comments: [summary(clean)], priorStatuses: [{ ...latch, creator: { login: 'github-actions[bot]', type: 'Bot' } }] });
  await run(refresh); assert.equal(result(refresh), 'failure');
});
test('a missing recorded summary fails even if its deletion event was not delivered', async () => {
  const fixture = client({ comments: [summary(clean)], priorStatuses: [watermark(43, '2026-10-23')] });
  await run(fixture); assert.equal(result(fixture), 'failure');
});
test('a newer authenticated bot update recovers a deletion latch', async () => {
  const fixture = client({ comments: [{ ...summary(clean), id: 44, updated_at: '2026-10-24' }],
    priorStatuses: [watermark(43, '2026-10-23', 'failure')] });
  await run(fixture); assert.equal(result(fixture), 'success');
});
test('partial pending publication cannot retain evaluated merge success', async () => {
  const fixture = client({ comments: [summary(clean)], statusError: value => value.sha === head });
  await assert.rejects(run(fixture), /status write failed/);
  assert.deepEqual(fixture.statuses.map(item => [item.sha, item.state]), [[merge, 'pending']]);
});
test('workflow-run dispatch preserves an inherited Actions repo getter', async () => {
  const fixture = client({ comments: [summary(clean)] });
  const original = Object.assign(Object.create({ get repo() { return { owner: 'hemsoft-dev', repo: 'codexbar' }; } }),
    { eventName: 'workflow_run', payload: {} });
  fixture.context = workflowRunContext(original, 1);
  await run(fixture); assert.equal(result(fixture), 'success');
});
