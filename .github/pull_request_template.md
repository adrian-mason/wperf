## Summary
<!-- 1-3 bullet points describing the change -->

## Authoritative Inputs
<!-- Which ADRs, design docs, or spec sections govern this PR?
     List each by name/number. Example:
       - ADR-002-supplement §3 (compat.bpf.h vendor pattern)
       - final-design.md §4.1 (wPRF header layout)
     If none apply, write: N/A (no governing spec) -->

## Deviations
<!-- Does this PR deviate from any accepted design or ADR?
     Only two valid states:
       - None
       - Deviation: <description> — doc update: <PR or commit ref>
     A deviation WITHOUT a prior or concurrent spec amendment is a process violation. -->

## Dependency Checklist
<!-- Required when adding or modifying direct dependencies -->
- [ ] New/modified dependency versions are aligned with baseline in `docs/lessons/REGISTRY.md`
- [ ] If deviating from baseline, rationale is documented in this PR description
- [ ] If introducing a new Cargo feature, CI validates both default and feature-on builds

## Review Checklist
<!-- For reviewers — check these IN ORDER before evaluating code correctness -->
- [ ] **Design conformance**: implementation matches the ADRs/specs listed in Authoritative Inputs
- [ ] **Deviations declared**: any spec drift is explicitly listed above with a doc update reference
- [ ] **Code correctness**: logic, error handling, edge cases
- [ ] **Tests**: new/changed code has adequate test coverage
- [ ] **CI green**: all required checks pass

## Test Plan
<!-- How was this tested? -->
