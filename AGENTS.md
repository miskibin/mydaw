# AGENTS.md

## Product Standard

WAVE Studio is a professional creative tool, not a demo. Solve the musician's
actual workflow problem rather than merely satisfying the literal surface of a
ticket. A smaller, complete feature is better than a broad but shallow one.

Do not ship placeholders, dead or misleading controls, disconnected mock UI, or
happy-path-only behavior. Every visible element must earn the space it consumes
and help the user understand or operate the product.

## Substantial Changes Require Research and Approval

A substantial change includes any new user workflow or feature, DSP behavior,
public API or data-model change, cross-boundary integration, production
dependency, or material UX, architecture, or performance decision.

Before implementing a substantial change:

1. Inspect the current implementation, relevant specs, tests, and architectural
   constraints. Trace the complete path the change would affect.
2. Research how strong products solve the underlying problem today.
3. Present the research in chat with sources, a recommended direction,
   alternatives, tradeoffs, edge cases, and the proposed acceptance criteria.
4. Wait for explicit user approval of the direction and scope.
5. After approval, implement the accepted scope autonomously. Ask again only if
   new evidence creates a materially different decision or expands the scope.

The agent may challenge the user's initial solution and proactively suggest a
better one. Clearly separate the requested scope from optional improvements,
and never implement an unapproved proactive addition.

Small, isolated corrections such as typos, obvious styling defects, or narrow
regression fixes may proceed after inspecting local context, without ceremonial
market research. If the classification is uncertain, treat the work as
substantial.

## Research Standard

Research must answer why a design is useful, not just show that a competitor has
it. Prefer current primary sources and verify version-sensitive claims.

- For DAW workflows, compare FL Studio and Ableton Live when relevant. Add the
  strongest task-specific references, such as Bitwig Studio, Logic Pro, REAPER,
  or a specialized tool. Do not force irrelevant comparisons.
- For effects, instruments, and plugin workflows, read official manuals and
  combine them with credible reviews and user discussions. Establish which
  products people value, why they value them, recurring frustrations, and the
  interaction or DSP details behind those opinions.
- For architecture and performance, prioritize specifications, primary
  technical documentation, measurements, and reproducible benchmarks over
  secondary summaries.
- Distinguish sourced evidence, inference, and subjective product preference.
  Where evidence conflicts or is weak, say so.
- Do not cargo-cult a competitor. Extract the user need, then design the best
  fit for WAVE Studio's architecture, constraints, and audience.

Unless the user asks for a permanent artifact, keep the research summary and
source links in chat rather than adding a research document to the repository.

## Product and UX Reasoning

For every substantial user-facing change, reason through the complete journey,
including:

- discoverability, information density, hierarchy, and efficient use of space;
- mouse, keyboard, focus, selection, drag, and undo/redo behavior;
- long names, deep hierarchies, large projects, and realistic data volumes;
- empty, loading, error, unavailable, disabled, and partial-success states;
- feedback, latency perception, accessibility, scaling, and contrast;
- consistency with existing components, tokens, gestures, and terminology.

Prefer information-bearing visuals and controls over decoration. Do not add an
icon, badge, animation, panel, or permanent control unless its user value
outweighs its visual and interaction cost.

## Architecture and Performance

Respect the existing boundaries. `ui/src/state.tsx` owns engine interaction;
views do not call Tauri directly. Keep Rust DTOs, the TypeScript `Backend`
contract, `TauriBackend`, and `MockBackend` behavior aligned. Extend existing
components and patterns instead of creating parallel implementations.

Treat performance as a product feature:

- Prefer lazy, incremental, and bounded work. Virtualize large collections when
  rendering cost would otherwise scale with the full data set.
- Cache expensive derived work only with an explicit cache key, invalidation
  policy, memory bound, and tests for stale-result behavior.
- Measure critical paths and representative worst cases. Do not optimize from
  intuition alone or move avoidable work into a per-frame loop.
- Keep the application responsive during scans, file operations, decoding, and
  other slow or failure-prone work.

The real-time audio callback must not allocate, lock, block, perform I/O, log,
panic, or do unbounded work. Preserve deterministic, glitch-free behavior across
supported sample rates and block sizes. Smooth continuous parameters and handle
discrete changes, bypass, graph swaps, and teardown without clicks or invalid
intermediate states.

## Implementation Standard

Implement an approved feature as a complete vertical slice wherever applicable:
engine and DSP, IPC DTOs and commands, TypeScript backend contracts,
`MockBackend`, state management, UI, project persistence, tests, and user-facing
documentation.

- Map failure modes and acceptance criteria before editing.
- Preserve unrelated user changes and established architectural invariants.
- Keep every drawn control wired to real behavior or visibly disabled with a
  truthful explanation.
- Add regression coverage for the main behavior, realistic edge cases, cache
  invalidation, and failure paths.
- Manually verify the actual user journey, not only isolated functions.
- For DSP or performance work, add objective measurements or deterministic test
  signals where practical.

## Verification and Definition of Done

Run the gates relevant to the touched areas. For a repository-wide or
cross-boundary change, run the full set:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
pnpm --dir ui test
pnpm --dir ui build
```

A change is done only when the approved workflow works end to end with realistic
data, relevant edge cases are handled, performance remains appropriate, and no
placeholder or integration gap remains.

In the final report, state:

- what the research changed or confirmed;
- what was implemented;
- what was tested or measured and with which commands;
- any approved limitation, deviation, or follow-up that remains.
