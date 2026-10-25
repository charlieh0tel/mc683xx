#!/bin/sh
# The commit-msg hook's checks (.githooks/commit-msg), on sample
# identities and messages through its --identities and --messages
# modes.  Run by `make hook-test`, part of `make ci`.
set -u

HOOK=$(dirname "$0")/../.githooks/commit-msg
SAMPLE=$(mktemp)
trap 'rm -f "$SAMPLE"' EXIT
failures=0

# expect pass|refuse MODE TEXT: the hook in MODE on TEXT passes or
# refuses it.
expect() {
    printf '%s\n' "$3" >"$SAMPLE"
    if sh "$HOOK" "--$2" "$SAMPLE" 2>/dev/null; then
        result=pass
    else
        result=refuse
    fi
    if [ "$result" != "$1" ]; then
        echo "FAIL: expected $1, got $result: --$2 '$3'" >&2
        failures=$((failures + 1))
    fi
}

expect pass identities 'Christopher Hoover <ch@murgatroid.com>'
expect pass identities 'Claude Monet <cm@example.com>'
expect pass identities 'Claude Debussy <claude@example.com>'
expect pass identities 'Jean-Claude Code <jc@example.com>'
expect pass identities 'Claude Codex Smith <cs@example.com>'
expect pass identities 'Ann Smith <ann@notanthropic.com>'
expect pass identities 'Ann Smith <anthropic.com@example.com>'
expect refuse identities 'Claude <noreply@anthropic.com>'
expect refuse identities 'Claude Code <x@example.com>'
expect refuse identities 'claude bot <x@example.com>'
expect refuse identities 'Claude Assistant <x@example.com>'
expect refuse identities 'Claude <x@example.com>'
expect refuse identities 'claude[bot] <x@example.com>'
expect refuse identities 'Ann Smith <ann@anthropic.com>'
expect refuse identities 'Ann Smith <ann@mail.Anthropic.com>'
expect refuse identities 'Anthropic Agent <x@example.com>'
# As `git var` gives an identity, with the time after the address.
expect refuse identities 'Claude Code <x@example.com> 1700000000 +0000'
expect pass identities 'Claude Monet <cm@example.com> 1700000000 +0000'
# One bot line among people refuses the lot.
expect refuse identities 'Claude Monet <cm@example.com>
Claude <noreply@anthropic.com>'

expect pass messages 'Fix the PIT prescaler'
expect pass messages 'Credit Claude Monet in the examples'
expect pass messages '# Co-Authored-By: an editor comment'
expect refuse messages 'Fix it

Co-Authored-By: Claude <noreply@anthropic.com>'
expect refuse messages 'Fix it

Generated with [Claude Code](https://claude.ai/code)'
expect refuse messages 'Fix it

Claude-Session: https://claude.ai/code/session_x'

if [ "$failures" -ne 0 ]; then
    echo "commit-msg tests: $failures failed" >&2
    exit 1
fi
