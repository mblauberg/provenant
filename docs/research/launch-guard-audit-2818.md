# Launch-preparation refusal audit for #2818

This is an integration inventory, not a claim that the guard audit is complete. Intrinsic process and backend representation limits must be distinguished from optional policy defaults.
The relevant owners are `confinement` (plan, permission validation, rendering,
anchors, Git and cache), `providers` (adapter admission, credential reads,
provider launch builders, lane setup and tool discovery), and `run/src/launch.rs`
(parent authority, attempt state, custody and handoff). The four boundary
classes below are those of ADR 0035.

| Refusal family and owner | Disposition | Boundary or open question |
|---|---|---|
| Credential roots and authentication fixture selection, `permissions/validation.rs`, `providers/*::prepare`, `run::child_access_within` | Kept | Credentials and authority; a selected executable, worktree or configured root still fails closed. |
| Git control hard links, repository revalidation, protected Git write authority and parent/child grant checks, `confinement::{repo,permissions}`, `providers::lane`, `run::launch` | Kept | Lane Git, object integrity and authority. |
| Changed or missing anchored paths, malformed or unrecorded permission profile, invalid selected identity or continuation, `confinement::{plan,permissions}`, `providers::{contract,launch}` | Kept | Object integrity or authority; no authority is reconstructed from a changed object. |
| Network reach and nested confinement, `confinement::{plan,permissions}` and `run::child_access_within` | Kept | Authority: a child cannot gain reach or claim an unenforced policy. |
| Optional discovered credential-root grants, `providers::launch::filter_optional_grants` | Loosened | Omit and record a warning; the required grant guard is unchanged. |
| User skills, `providers::skill_stage` and CLI preparation | Loosened | A selected tree is staged; credential-linked, cyclic, changing, oversized and unsupported entries are skipped with warnings. A staging write failure retries without skills and warns. |
| Invalid project exec and shared roots, `cli::run::provider::project_paths` | Loosened | Ignore the entry and record its key and reason; no unvalidated grant is applied. |
| Unsupported capability or invalid selected executable, prompt/argument shape, `providers::{contract,launch,agy,cursor,command}` | Open | Distinguish authority and process requirements from configurable policy. `Prompt::check_argv` caps argv text at 64 KiB; top-level launches spill larger prompts to an attempt file, while nested argv launches still refuse. The bound protects process arguments, but its fixed threshold and nested fallback need assessment. |
| Profile size and grant bounds, `permissions::{render,validation}` | Open | `ProfileTooLarge` and `BoundExceeded` still refuse; distinguish OS transport limits from optional discoveries and expose configurable bounds where possible. |
| Non-UTF-8 SBPL path, `permissions::render` | Kept | Intrinsic backend representation limitation: the current SBPL encoder cannot express this path losslessly. |
| Cache traversal depth and sweep failures, `confinement::cache` | Outside scope | Cache GC reports these after a sweep; they are not launch preparation. |
| Backend unavailable, probe cancelled, process execution and setup I/O, `confinement::{permissions,plan}` and `run::launch` | Open | Distinguish unavailable enforcement or uncertain handoff (authority/integrity) from optional preparation I/O. |

The open families above prevent claiming the #2818 guard-audit acceptance as
complete. A line-by-line disposition of the remaining propagation sites is
still required; neither an error type nor a filename alone establishes a
boundary class. The unchanged guard and native escape tests remain mandatory.
