# Launch-preparation refusal audit for #2818

This inventory classifies launch refusals by their cause. `operational` marks
setup failures that stop preparation without asserting an ADR 0035 security
boundary. Resource bounds are implementation limits, not claims about intrinsic
operating-system limits.
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
| Unsupported capability or invalid selected executable, prompt/argument shape, `providers::{contract,launch,agy,cursor,command}` | Classified | Capability refusal preserves the selected adapter’s declared authority; an invalid executable is an unavailable required input. `ProviderError::PromptTooLarge` is a fixed process-argument resource bound, not an OS maximum: top-level launches spill to a file, while nested argv launches refuse rather than silently change their contract. |
| Profile size and grant bounds, `permissions::{render,validation}` | Configurable | Host `domains.run.profile_limit_bytes` and `domains.run.grant_limit` default to 512 KiB and 128 roots and can be raised; they are resource defaults, not OS limits. The separate Git control-directory scan bound preserves complete integrity validation. |
| Non-UTF-8 SBPL path, `permissions::render` | Kept | Intrinsic backend representation limitation: the current SBPL encoder cannot express this path losslessly. |
| Cache traversal depth and sweep failures, `confinement::cache` | Outside scope | Cache GC reports these after a sweep; they are not launch preparation. |
| Backend unavailable, probe cancelled, process execution and setup I/O, `confinement::{permissions,plan}` and `run::launch` | Classified | `PermissionError::{BackendUnavailable, BackendNotAvailable, NestedConfinementUnsupported}` are enforcement/authority refusals. `PermissionError::Cancelled` and uncertain process handoff preserve custody and object integrity; `PermissionError::{Execution, Filesystem}` and required setup I/O fail preparation rather than claiming a launch. Optional skill staging I/O is handled by the warning and retry described above. |

The unchanged guard and native escape tests remain mandatory. The inventory is
complete for the current launch preparation and handoff paths; operational
setup refusals are distinct from security boundaries and configurable defaults.
