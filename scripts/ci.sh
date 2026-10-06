#!/bin/sh
# Run CI (default) or a release dry run on demand and watch it.
#   scripts/ci.sh            -> ci.yml on the current branch
#   scripts/ci.sh release    -> release.yml dry run (builds binaries, no release)
set -eu
wf="${1:-ci}.yml"
ref=$(git rev-parse --abbrev-ref HEAD)
gh workflow run "$wf" --ref "$ref"
sleep 5
id=$(gh run list --workflow="$wf" --branch "$ref" -L 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$id" --exit-status
