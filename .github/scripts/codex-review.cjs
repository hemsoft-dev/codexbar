'use strict';

const CONTEXT = 'codex-review';
const MAINTAINER_ID = 8227352;
const CODEX = 'chatgpt-codex-connector[bot]';

// Only a structured result from the connected app can satisfy the ordinary gate.
function codexVerdict(comments, head) {
  const summaries = comments.filter(comment => comment.user?.login === CODEX && comment.user?.type === 'Bot')
    .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
  for (const comment of summaries) {
    if (!comment.body?.includes('<!-- codex-review-state:v1')) continue;
    const malformed = { state: 'failure', description: 'The latest Codex summary is malformed' };
    const match = comment.body.match(/<!-- codex-review-state:v1 (\{[^\n]*\}) -->/);
    if (!match) return malformed;
    let review;
    try { review = JSON.parse(match[1]).code_review; } catch { return malformed; }
    if (!review || typeof review.commit_sha !== 'string') return malformed;
    if (review.commit_sha !== head) continue;
    if (review.status !== 'completed') return { state: 'pending', description: 'Current-head Codex review is running' };
    if (review.has_any_findings === false && review.has_unresolved_findings === false) {
      return { state: 'success', description: 'Codex reviewed this exact head with no findings', target_url: comment.html_url };
    }
    return { state: 'failure', description: 'Codex findings or an incomplete verdict need attention', target_url: comment.html_url };
  }
  return { state: 'pending', description: 'A clean Codex review of this exact head is required' };
}

// A maintainer-published cross-model review is the documented alternative when
// connected Codex is unavailable. It is review evidence, never a bypass flag.
function fallbackReceipt(review, head) {
  if (review.user?.id !== MAINTAINER_ID || review.user?.type !== 'User' ||
      review.commit_id !== head || review.state !== 'COMMENTED') return false;
  const match = review.body?.match(/^<!-- codexbar-review-fallback:v1 (\{[^\n]*\}) -->/);
  if (!match) return false;
  try {
    const receipt = JSON.parse(match[1]);
    return receipt.head === head && receipt.clean === true && receipt.provider === 'claude' &&
      receipt.model === 'claude-opus-5-5' && ['hung', 'unavailable'].includes(receipt.codex);
  } catch { return false; }
}

function eligibleEvent(context) {
  if (context.eventName === 'issue_comment') {
    const comment = context.payload.comment;
    return !!context.payload.issue?.pull_request && comment?.user?.login === CODEX &&
      comment.user.type === 'Bot' && !!comment.body?.includes('<!-- codex-review-state:v1');
  }
  if (context.eventName === 'pull_request_review') return context.payload.review?.user?.id === MAINTAINER_ID;
  return true;
}

function triggerIncluded(context, comments, reviews) {
  const event = context.payload;
  const comment = event.comment;
  if (comment?.user?.login === CODEX && comment.user.type === 'Bot') {
    const actual = comments.find(item => item.id === comment.id);
    if (event.action === 'deleted') return !actual;
    if (!actual) return false;
    if (Date.parse(actual.updated_at) > Date.parse(comment.updated_at)) return true;
    return actual.updated_at === comment.updated_at && actual.body === comment.body;
  }
  const review = event.review;
  if (review?.user?.id === MAINTAINER_ID) {
    const actual = reviews.find(item => item.id === review.id);
    return actual?.body === review.body && actual?.state.toUpperCase() === review.state.toUpperCase();
  }
  return true;
}

async function snapshot(github, owner, repo, number) {
  const comments = await github.paginate(github.rest.issues.listComments, { owner, repo, issue_number: number, per_page: 100 });
  const reviews = await github.paginate(github.rest.pulls.listReviews, { owner, repo, pull_number: number, per_page: 100 });
  return { comments, reviews };
}

// Compare all mutable fields that can affect the decision, including deletion
// and later maintainer reviews. Ordinary conversation cannot invalidate it.
function fingerprint({ comments, reviews }) {
  return JSON.stringify({
    comments: comments.filter(item => item.user?.login === CODEX).map(item =>
      [item.id, item.user, item.updated_at, item.body]),
    reviews: reviews.filter(item => item.user?.id === MAINTAINER_ID).map(item =>
      [item.id, item.user, item.commit_id, item.state, item.body])
  });
}

async function run({ github, context, core }) {
  if (!eligibleEvent(context)) return;
  const { owner, repo } = context.repo;
  const number = context.payload.pull_request?.number ?? context.payload.issue?.number ?? Number(context.payload.inputs?.number);
  if (!Number.isSafeInteger(number) || number <= 0) throw new Error('A pull request number is required');
  const { data: pr } = await github.rest.pulls.get({ owner, repo, pull_number: number });
  if (pr.state !== 'open' || pr.base.ref !== 'main') return;
  const head = pr.head.sha;
  const merge = pr.merge_commit_sha;
  const shas = [...new Set([head, merge].filter(Boolean))];
  const publish = async verdict => {
    for (const sha of shas) {
      await github.rest.repos.createCommitStatus({ owner, repo, sha, context: CONTEXT, ...verdict });
    }
  };
  // Revoke previous success before reading mutable evidence. An API failure
  // therefore leaves the gate pending rather than retaining cached consent.
  await publish({ state: 'pending', description: 'Revalidating current-head review evidence' });
  const first = await snapshot(github, owner, repo, number);
  let verdict = codexVerdict(first.comments, head);
  const latest = first.reviews.filter(review => review.user?.id === MAINTAINER_ID && review.commit_id === head).at(-1);
  const fallback = latest && fallbackReceipt(latest, head) ? latest : null;
  const fresh = await snapshot(github, owner, repo, number);
  const unchanged = triggerIncluded(context, fresh.comments, fresh.reviews) && fingerprint(first) === fingerprint(fresh);
  if (!unchanged) {
    verdict = { state: 'pending', description: 'Review evidence changed; a fresh event must revalidate it' };
  } else if (fallback && verdict.state === 'pending') {
    const { data: permission } = await github.rest.repos.getCollaboratorPermissionLevel({ owner, repo, username: fallback.user.login });
    if (['admin', 'maintain'].includes(permission.permission)) {
      verdict = { state: 'success', description: 'Clean current-head Claude Opus fallback review', target_url: fallback.html_url };
    }
  }
  if (!merge) verdict = { state: 'pending', description: 'Waiting for GitHub to compute the test merge commit' };
  // Head evidence stays bound to the corresponding test merge revision.
  const { data: live } = await github.rest.pulls.get({ owner, repo, pull_number: number });
  if (live.state !== 'open' || live.head.sha !== head || live.merge_commit_sha !== merge) {
    core.info('Head or test merge changed; no final verdict was published');
    return;
  }
  await publish(verdict);
  core.info(`${CONTEXT} ${head} / ${merge}: ${verdict.state}`);
}

module.exports = { codexVerdict, fallbackReceipt, eligibleEvent, triggerIncluded, run };
