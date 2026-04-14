## Summary
<!-- 1-3 bullet points describing the change -->

## Authoritative Inputs
<!-- Which ADRs, design docs, or spec sections govern this PR?
     List each by name/number. Example:
       - ADR-002-supplement §3 (compat.bpf.h vendor pattern)
       - final-design.md §4.1 (wPRF header layout)
     If none apply, write: None (no governing spec) -->

## Deviations
<!-- Does this PR deviate from any accepted design or ADR?
     Valid entries:
       - None
       - Deviation: <description> — doc update: <PR or commit ref>
     A deviation WITHOUT a prior or concurrent spec amendment is a process violation. -->

## Dependency Checklist
<!-- Required when adding or modifying direct dependencies -->
- [ ] New/modified dependency versions are aligned with baseline in `docs/lessons/REGISTRY.md`
- [ ] If deviating from baseline, rationale is documented in this PR description
- [ ] If introducing a new Cargo feature, CI validates both default and feature-on builds

## Review Checklist
<!-- For reviewers — check these items in order, prioritizing design conformance -->
- [ ] **Design conformance**: implementation matches the ADRs/specs listed in Authoritative Inputs
- [ ] **Deviations declared**: any spec drift is explicitly listed above with a doc update reference
- [ ] **Code correctness**: logic, error handling, edge cases
- [ ] **Tests**: new/changed code has adequate test coverage
- [ ] **CI green**: all required checks pass (Directive 6 — verify before issuing PASS)
- [ ] **External reviews**: Gemini and all automated reviewer comments addressed (Directive 6)
- [ ] **Owner approval**: @Adrian-Mason explicit merge authorization (Directive 6)

## Runtime Evidence
<!-- Required for E2E validation and gate-critical PRs (Oracle Directive 5).
     Paste actual command output showing the test ran and produced expected results.
     For BPF/kernel PRs: probe attach confirmation, event counts, relevant excerpts.
     For gate-critical PRs: evidence must demonstrate the gate claim is satisfied.
     For non-E2E PRs: write "N/A — not E2E or gate-critical" -->

## Test Plan
<!-- How was this tested? -->
