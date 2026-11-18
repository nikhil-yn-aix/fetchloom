#!/usr/bin/env bash
# says whether ci.yml already concluded success on the commit `sha` in a run for a pull request.
# main fast-forwarded to a pull request's head points at exactly the commit, and so the tree and
# the workflow, that run proved. writes found=true and the run, or found=false, to $GITHUB_OUTPUT
# where it is set, else to stdout. a question the api cannot answer counts as not found, so every
# job runs.
set -euo pipefail

usage="usage: ci-verified.sh <owner/repository> <commit sha>"
repository=${1:?$usage}
sha=${2:?$usage}
if [[ ! $repository =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ || ! $sha =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::$usage, the sha in full"
  exit 2
fi
output=${GITHUB_OUTPUT:-/dev/stdout}

if ! run=$(gh api --method GET "repos/$repository/actions/workflows/ci.yml/runs" \
  -f head_sha="$sha" -f event=pull_request -f status=success -f per_page=100 \
  --jq "[.workflow_runs[] | select(.head_sha == \"$sha\" and .event == \"pull_request\" and .conclusion == \"success\")][0].html_url // empty"); then
  echo "::warning::github's api did not say which runs of ci.yml passed on $sha, so every job runs"
  echo "found=false" >> "$output"
  exit 0
fi

if [[ -n $run ]]; then
  echo "ci passed on $sha in the pull request run $run, so this run checks nothing again"
  printf 'found=true\nrun=%s\n' "$run" >> "$output"
else
  echo "no pull request run of ci passed on $sha, so every job runs"
  echo "found=false" >> "$output"
fi
