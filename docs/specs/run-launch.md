# Run launch and admission

Owner: `crates/run`, `crates/confinement`, `crates/providers`
Anchor: `crates/run/src/launch.rs`, `crates/run/src/admission.rs`

Test paths are relative to `tools/front-door/`.

## Admission and custody

Admission commits the reservation and attempt together. A verified worktree has
at most one writer; resource probes do not hold the shared registry lock or run
inside the transaction. Busy capacity is not a queue. Uncertain custody keeps
its ownership until release is proved.
- Why: admission must not create duplicate writers or hold coordination locks across external work.
- Test: `crates/run/src/dispatch_disk_admission_tests.rs::resource_pressure_and_busy_caps_never_queue_default_admission`

Permanent pre-admission refusals are durable outcomes of their operation. Replay
returns the refusal without preparing again; a failure to store it is not
acknowledged as success.
- Why: a lost response must not turn a refusal into another launch.
- Test: `tests/refused_operation.rs::dirty_writer_refusal_is_durable_visible_and_notified_once`

Terminal attempts are immutable. A retry is a new linked attempt, and a writer
is never replayed over its edits. A terminal attempt has at most one successor.
- Why: continuation must preserve both history and exclusive custody.
- Test: `crates/run/src/dispatch_lifecycle_tests.rs::resume_continues_a_terminal_attempt_once_with_its_route`

## Policy and confinement

Launch policy defaults to refusal only at credentials, Git isolation, data
integrity and authority boundaries. Other checks allow or warn unless configured
otherwise. A write-scope refusal happens before publication and retains private
work.
- Why: advisory checks must not become accidental authority boundaries.
- Test: none (review gate)

The selected sandbox floor applies to each candidate and is sealed for
continuations. Unsupported confinement is reported rather than represented as
sandboxed execution.
- Why: a retry must not silently weaken the requested boundary.
- Test: `crates/run/src/launch.rs::policy_reporting_tests::continuation_keeps_the_sealed_parent_floor_and_policy`

Read-only write exceptions come from user-owned configuration, remain within
verified repository paths and are recorded on the attempt. Git and provider
control paths are excluded.
- Why: only the user may widen a read-only lane's write authority.
- Test: `crates/confinement/src/plan/read_only.rs::policy_defaults_and_refuses_escape_control_paths_and_symlinks`

Host-native launches retain confinement and provenance and cannot resume a
provider session or select model-specific controls.
- Why: native execution must not claim a provider contract it cannot satisfy.
- Test: `crates/records/src/adapters.rs::host_native_declares_a_fresh_named_writer_contract`

## Inputs and resources

Unreadable required host inputs reject an attempt before launch. A pinned source
view reports its failure without exposing captured contents or credentials.
- Why: a partial source view must not be mistaken for the requested input.
- Test: `crates/run/src/dispatch_custody_tests.rs::missing_stored_prompt_ends_the_attempt_rejected_with_the_cause`

Lane download stores do not copy the host registry, and fresh provider homes do
not carry provider history.
- Why: shared build inputs must not disclose host or provider state.
- Test: `crates/run/src/cargo_seed.rs::download_stores_are_shared_without_reading_host_registry`; `crates/providers/src/tests_lane_toolchains.rs::fresh_codex_home_setup_never_includes_provider_history`

Build output is collected only when its Cargo locks are free and recent writes
are protected. Priority adjustment failure does not refuse launch.
- Why: resource management must not corrupt active work or prevent a launch.
- Test: `crates/run/src/build_dir_gc.rs::a_held_lock_prevents_deletion_whichever_of_the_three_is_held`; `crates/supervisor/src/process_os/provider.rs::provider_and_descendant_inherit_priority`
