'use strict';

const CONTEXT = 'codex-review';
const CODEX = 'chatgpt-codex-connector[bot]';

// Only a structured result from the connected app can satisfy the ordinary gate.
function codexVerdict(comments, head) {
  const summaries = comments.filter(comment => comment.user?.login === CODEX && comment.user?.type === 'Bot')
    .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
  for (const comment of summaries) {
    const match = comment.body?.match(/<!-- codex-review-state:v1 (\{[^\n]*\}) -->/);
    if (!match) continue;
    let review;
    try { review = JSON.parse(match[1]).code_review; } catch { continue; }
    if (review?.commit_sha !== head) continue;
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
  if (review.user?.login !== 'HemSoft' || review.user?.type !== 'User' ||
      review.commit_id !== head || review.state !== 'COMMENTED') return false;
  const match = review.body?.match(/^<!-- codexbar-review-fallback:v1 (\{[^\n]*\}) -->/);
  if (!match) return false;
  try {
    const receipt = JSON.parse(match[1]);
    return receipt.head === head && receipt.clean === true && receipt.provider === 'claude' &&
      receipt.model === 'claude-opus-5-5' && ['hung', 'unavailable'].includes(receipt.codex);
  } catch { return false; }
}

async function run({ github, context, core }) {
  const { owner, repo } = context.repo;
  const number = context.payload.pull_request?.number ?? context.payload.issue?.number ?? Number(context.payload.inputs?.number);
  if (!Number.isSafeInteger(number) || number <= 0) throw new Error('A pull request number is required');
  const { data: pr } = await github.rest.pulls.get({ owner, repo, pull_number: number });
  if (pr.state !== 'open' || pr.base.ref !== 'main') return;
  const head = pr.head.sha;
  const comments = await github.paginate(github.rest.issues.listComments, { owner, repo, issue_number: number, per_page: 100 });
  let verdict = codexVerdict(comments, head);
  const reviews = await github.paginate(github.rest.pulls.listReviews, { owner, repo, pull_number: number, per_page: 100 });
  const latest = reviews.filter(review => review.user?.login === 'HemSoft' && review.commit_id === head).at(-1);
  const fallback = latest && fallbackReceipt(latest, head) ? latest : null;
  if (fallback && verdict.state === 'pending') {
    const { data: permission } = await github.rest.repos.getCollaboratorPermissionLevel({ owner, repo, username: 'HemSoft' });
    if (['admin', 'maintain'].includes(permission.permission)) {
      verdict = { state: 'success', description: 'Clean current-head Claude Opus fallback review', target_url: fallback.html_url };
    }
  }
  // Never attach a decision assembled across a concurrent head change.
  const { data: live } = await github.rest.pulls.get({ owner, repo, pull_number: number });
  if (live.state !== 'open' || live.head.sha !== head) {
    core.info('Head changed while reading reviews; the synchronize event owns the new head');
    return;
  }
  await github.rest.repos.createCommitStatus({ owner, repo, sha: head, context: CONTEXT, ...verdict });
  core.info(`${CONTEXT} ${head}: ${verdict.state}`);
}

module.exports = { codexVerdict, fallbackReceipt, run };
