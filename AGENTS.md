# AGENTS.md

Reusable working preferences for coding agents. Discover project-specific commands, architecture, and conventions from the current repository.

## Required skills

Use the installed `clean-code` skill when writing or reviewing code. Use the installed `i-have-adhd` skill to shape replies.

## Understand before changing

- Read the relevant code and trace the complete user flow before proposing a fix. Account for existing validation, state transitions, background work, and cleanup.
- Establish which behavior the user wants to preserve. Do not remove a useful feature merely because removing it makes an edge case easier to handle.
- Distinguish observed behavior, intended behavior, and assumptions. Verify documentation against the current implementation and call out disagreements.
- Explain bugs through a concrete trigger and consequence. State likelihood separately from impact; do not present a rare scenario as the normal flow or dismiss a serious issue only because it is rare.
- Ask a focused question only when missing information materially changes the solution. Use available code and conversation context first.

## Make the smallest useful change

- Extend existing functions, services, and patterns before adding new abstractions or files. Explain why an existing location cannot hold a proposed addition.
- Prefer a focused fix within the established flow. Do not introduce a new protocol, verification step, dependency, or subsystem without a demonstrated need.
- Do not add unrequested guards, caps, kill switches, or configuration keys. Propose new product restrictions before implementing them.
- Show a diff and wait for "apply" before editing code. Clearly identify behavior changes, feature removal, and remaining limitations in the proposal.
- UI changes are pre-authorized; implement them directly without waiting for "apply."
- Keep unrelated refactoring out of a bug fix. Preserve the user's existing changes and follow the project's naming, error handling, localization, and testing conventions.

## Write and verify carefully

- Remove agent-created temporary tests after verification unless the user explicitly asks to keep them. Preserve existing project tests.
- Use clear names, small focused functions, and minimal comments that explain why a non-obvious choice exists.
- Write new comments and test names in short, simple English. Do not edit, translate, or delete existing Chinese comments; add a short English note when needed.
- Do not add comments about matching another app or implementation history. Explain behavior or a non-obvious reason only when needed.
- Describe behavior in comments and test names rather than using ticket numbers or external document section numbers.
- Discover the runtime, package manager, and validation commands from project configuration and documentation. Inspect test setup before running suites that may contact live services, mutate shared data, or terminate the runner.
- Run checks appropriate to the change, including the project's typecheck when applicable. Prefer scoped tests. Report failures honestly, distinguish existing failures from regressions, and do not claim unrun checks passed.

## Communicate and maintain guidance

- Lead with the result or next action. Keep replies concise and easy to scan; use short numbered lists when helpful.
- Explain what changed, why it helps, and what remains unfinished. Distinguish a risk reduction from a complete guarantee.
- When corrected, update the approach and preserve the user's stated preferences. Avoid repeatedly proposing a rejected redesign.
- When matching a reference app, follow its verified behavior. Discuss differences in fee displays or validation rules before substituting another approach.
- Keep corresponding mobile and extension code structurally similar. Preserve inline logic and guard order instead of extracting helpers solely for refactoring.
- Preserve the extension's transaction status labels and progress steps when matching mobile flows.
- Show loading only when the corresponding request is eligible and pending; missing data alone does not mean a request is running.
- Do not add request-ID refs or counters for balance refreshes. The user rejected this approach; keep claim status and refresh handling simple.
- Save durable working lessons here. Keep changing architecture details, filenames, vendor rules, commands, and task status in the project's own documentation, and verify them when needed.

# Model Behavior Constraints
- DO NOT use inline Python scripts, perl, or heredocs to edit files.
- ALWAYS use the native `apply_patch` or structured file-editing tools.
- If an edit fails, re-read the file instead of writing a script to modify it.
