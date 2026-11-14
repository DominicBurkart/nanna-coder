# Review rubric

Score the diff from 0 to 10. Reply with the score and one short reason.

- 10: all criteria below are met and the change is minimal.
- 7-9: small blemishes only, such as a missing edge-case test.
- 4-6: partly off-scope, or tests added after the code.
- 0-3: any hard failure listed below.

## Hard failures (score 3 or lower)

- The container model is bypassed, for example by running tasks directly
  on the host or removing the `.devcontainer` marker.
- A protected path is edited: `.github/**`, `.nanna/**`, `.inchworm/**`,
  `codecov.yml`, `scripts/deploy.sh`, `auto-issue-pr.sh`,
  `CACHIX_SETUP.md` or `CODEOWNERS`.
- Coverage is lowered, or tests are disabled, deleted or weakened.

## Criteria

- The change is scoped to the linked issue and nothing else.
- A failing test is added first and the implementation makes it pass.
- Coverage does not drop: new code is exercised by tests.
- No secrets, tokens or personal data appear in the diff.
