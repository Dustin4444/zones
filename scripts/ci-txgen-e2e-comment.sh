#!/usr/bin/env bash
set -euo pipefail

marker='<!-- txgen-e2e-spam -->'
body="${COMMENT_BODY:-}"
if [ -z "$body" ]; then
  body="### Txgen L2 transfer spam

❌ The CI job ended before it produced a summary.

[Workflow run](${RUN_URL:?RUN_URL is required})"
fi
body="$marker
$body"

comment_id="$(gh api \
  "repos/$GITHUB_REPOSITORY/issues/${PR_NUMBER:?PR_NUMBER is required}/comments" \
  --jq '[.[] | select(.body | contains("<!-- txgen-e2e-spam -->"))] | first | .id // empty')"

if [ -n "$comment_id" ]; then
  gh api --method PATCH \
    "repos/$GITHUB_REPOSITORY/issues/comments/$comment_id" \
    -f body="$body" >/dev/null
else
  gh api --method POST \
    "repos/$GITHUB_REPOSITORY/issues/$PR_NUMBER/comments" \
    -f body="$body" >/dev/null
fi
