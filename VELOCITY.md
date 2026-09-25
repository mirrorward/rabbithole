# Working with Velocity

RabbitHole uses [Velocity](https://velocity.quest/rabbithole/issues?team=RH) for
actionable work. Use the **RabbitHole** workspace and **RabbitHole (`RH`)** team.
`PLAN.md` describes the product direction; `TODO.md` records wave-level progress.
Keep those documents consistent with completed work without treating a historical
checkbox or comment as proof of the current implementation.

## Connection

Use the configured `velocity` MCP server at `https://velocity.quest/api/mcp`.
Credentials belong in private MCP configuration, never in this repository,
issue descriptions, comments, or command output. Confirm the active workspace
and team before making changes. Discover current status IDs through Velocity;
do not assume an ID from another workspace is valid here.

## Creating issues

1. Search existing issues, including Done and Cancelled items, for the same
   behavior or deliverable. Update or reopen the existing issue when appropriate
   rather than creating a duplicate.
2. Inspect the current code and relevant roadmap entry. Exclude completed work
   and intentional behavior; distinguish an implementation task from an owner
   decision, optional proposal, or external dependency.
3. Create one issue per independently deliverable outcome. Use a concrete title;
   preserve the roadmap wave prefix (`[W8]`, for example) when applicable.
4. Describe the problem, current behavior, intended result, scope, and acceptance
   criteria. Include repository-relative file paths and line references, a
   relevant TODO/PLAN entry where applicable, and reproduction steps or evidence
   when available.
   Record dependencies and decisions explicitly; link related issues instead of
   duplicating their scope.
5. Put new, untriaged work in **Backlog**, with no assignee or priority unless
   those choices are established. Use **Todo** for work selected as ready to
   start. Do not invent owners, deadlines, or urgency.
6. Read the saved issue back and confirm its workspace, team, description, and
   status. Record its `RH-…` identifier for later commits and comments. After an
   ambiguous creation response, search for the saved issue before retrying.

Suggested description:

```markdown
## Problem and intended behavior
What happens today, when it happens, and what should happen instead.

## Acceptance criteria
- Observable behavior required for completion.
- Relevant failure, compatibility, and recovery cases.

## Evidence and dependencies
- Source: relative/path.rs:123; TODO.md:456.
- Related issues, required decisions, and external prerequisites.
```

## Working on an issue

1. Read the issue, its comments, dependencies, and applicable repository
   instructions. Recheck the reported gap against the current working tree.
   Prefer ready, bounded work and avoid duplicating another contributor's work.
2. Move the issue to **In Progress** when implementation starts. Post a pickup
   comment with the intended scope and validation plan. Assign an owner only
   when their identity and responsibility are established.
3. Keep changes focused on the acceptance criteria. Preserve unrelated local
   changes; do not include them in the issue's commit. Follow the user's branch,
   review, and publication instructions. Use the issue identifier in the commit
   message and in a PR title or description when a PR is part of the workflow.
4. Run checks appropriate to the change and review the resulting diff. Test
   meaningful behavior and failure cases. Distinguish local checks, fixtures,
   CI, deployment, and live verification; none automatically proves the others.
5. **Post a comment whenever meaningful work is completed on the issue**, even
   if the entire issue remains unfinished. Describe what changed, validation
   results, remaining work or blockers, and relevant commit/PR/CI links. Also
   comment when scope changes, a blocker is found, or work is handed off. Do not
   flood the issue with repetitive polling updates.
6. Keep partial or blocked work open. Explain the blocker and next step in a
   comment; use a blocked status if the team provides one. If additional
   independent work is discovered, create and link a follow-up issue rather
   than silently expanding this issue or claiming partial work is complete.

## Completion gate: commit, push, CI, then Done

An issue may be marked **Done** only after all of the following are confirmed:

- Its acceptance criteria are satisfied and the final diff has been reviewed.
- Appropriate local checks pass.
- The relevant changes are committed, and that commit is pushed to the intended
  remote branch. Verify the remote contains the commit.
- All applicable CI jobs for the **final pushed changes** finish successfully.
  For branch runs, verify the tested commit is the pushed SHA. For PR runs that
  test a synthetic merge commit, verify the run includes the latest pushed PR
  head and record both the head SHA and tested merge SHA. Check actual run and
  job conclusions; queued/running jobs, an older green run, cancelled checks,
  or local test success do not satisfy this gate.
- Any merge, deployment, external compatibility test, or live verification
  explicitly required by the issue is also complete.

The current [CI workflow](.github/workflows/ci.yml) runs on pushes to `main` and
`claude/**`, and on pull requests. A push to another branch alone does not
trigger it. Use a PR when that matches the authorized workflow, or resolve the
CI trigger before claiming completion; do not change branches just to evade the
user's branch instructions.

If pushing is blocked, CI fails or is unavailable, or required checks never
start, leave the issue open and comment with the evidence and next step. Resolve
the problem or obtain an explicit workflow decision; do not silently waive the
gate or mark the issue Done because implementation appears finished.

After the gate passes, post a completion comment containing:

- The delivered behavior and how it satisfies the acceptance criteria.
- The commit SHA and remote commit or PR link.
- Local validation results and successful CI run links identifying the tested
  commit (and PR head/merge SHAs when applicable).
- Required live verification results and any remaining limitations or linked
  follow-ups that do not prevent acceptance.

Then set the issue to **Done** and read it back to verify the saved status and
comment. If a later regression invalidates completion, reopen it or create a
clearly linked regression issue, with evidence.
