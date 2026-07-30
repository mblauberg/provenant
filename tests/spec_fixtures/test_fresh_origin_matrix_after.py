#!/usr/bin/env python3
"""Focused CAPA-001 fresh-origin/scope-admission matrix extension.

This Python-stdlib oracle extends ``test_fresh_origin_after.py`` without
changing that defect witness.  It adds the matrix arms that require complete
fresh apply/post-state and file-backed startup/hydration behaviour.
"""

from __future__ import annotations

from copy import deepcopy
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
from typing import Any, Callable, Iterable

from assert_fk import ForeignKeySpec, assert_fk_rejected
import test_fresh_origin_after as core


Case = tuple[str, Callable[[], None]]
CASES: list[Case] = []


def case(case_id: str) -> Callable[[Callable[[], None]], Callable[[], None]]:
    def register(function: Callable[[], None]) -> Callable[[], None]:
        CASES.append((case_id, function))
        return function

    return register


def require(condition: bool, message: str) -> None:
    if not condition:
        raise core.OracleFailure(message)


def changed(value: Any) -> Any:
    if value is None:
        return "crossed"
    if isinstance(value, bool):
        return not value
    if isinstance(value, int):
        return value + 1
    if isinstance(value, str):
        return value + "-crossed"
    if isinstance(value, dict):
        return {**value, "crossed": True}
    if isinstance(value, list):
        return [*value, "crossed"]
    raise TypeError(type(value).__name__)


def exact_object(value: Any, keys: Iterable[str], name: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{name} is not an object")
    core.exact_keys(value, keys, name)
    return value


BEFORE_REF_KEYS = (
    "kind",
    "handoffId",
    "handoffDigest",
    "preparationId",
    "preparationDigest",
    "plannedApplyId",
    "sourceMode",
    "recoverySource",
    "sourceJournalDigest",
    "freshApplyPlanDigest",
)
HANDOFF_REF_KEYS = (
    "handoffId",
    "handoffDigest",
    "preparationId",
    "preparationDigest",
    "sourceMode",
    "freshApplyPlanDigest",
)


def owner_ref(custody_id: str, revision: int, semantic: str, source: str) -> dict[str, Any]:
    return {
        "kind": "custody",
        "custodyRef": {
            "schemaVersion": 1,
            "runId": "run-001",
            "agentId": "agent-001",
            "custodyId": custody_id,
            "custodyRevision": revision,
        },
        "semanticDigest": semantic,
        "sourceRefDigest": source,
    }


def open_loss_vector() -> dict[str, Any]:
    before_ref = {
        "schemaVersion": 1,
        "runId": "run-001",
        "agentId": "agent-001",
        "generationLossId": "loss-open-001",
        "generationLossRevision": 3,
    }
    after_ref = {**before_ref, "generationLossRevision": 4}
    recovery_source = {
        "schemaVersion": 1,
        "kind": "generation-loss",
        "runId": "run-001",
        "agentId": "agent-001",
        "generationLossId": "loss-open-001",
        "generationLossRevision": 3,
    }
    new_custody_ref = {
        "schemaVersion": 1,
        "runId": "run-001",
        "agentId": "agent-001",
        "custodyId": "custody-open-fresh",
        "custodyRevision": 1,
    }
    handoff = {
        "handoffId": "handoff-open-001",
        "handoffDigest": core.D22,
        "preparationId": "preparation-open-001",
        "preparationDigest": core.D00,
        "plannedApplyId": "apply-open-001",
        "sourceMode": "open-generation-loss",
        "recoverySource": recovery_source,
        "sourceJournalDigest": core.D11,
        "freshApplyPlanDigest": core.D44,
    }
    effect = {
        "schemaVersion": 1,
        "effectKind": "fresh-origin",
        "role": "primary",
        "sourceMode": "open-generation-loss",
        "recoverySource": recovery_source,
        "sourceJournalDigest": core.D11,
        "freshHandoffDigest": core.D22,
        "admissionDigest": core.D33,
        "freshApplyPlanDigest": core.D44,
        "newCustodyRef": new_custody_ref,
        "newCustodySemanticDigest": core.D55,
        "newCustodySourceRefDigest": core.D66,
        "affectedGenerationLossBeforeRef": before_ref,
        "affectedGenerationLossBeforeJournalDigest": core.D11,
        "affectedGenerationLossAfterRef": after_ref,
        "affectedGenerationLossAfterSemanticDigest": core.D77,
    }
    effect_digest = core.ld("lifecycle-effect", effect)
    effects_set_digest = core.ld("effect-set", [effect_digest])
    replay = {
        "schemaVersion": 1,
        "transactionId": "apply-open-001",
        "projectSessionId": "session-001",
        "runId": "run-001",
        "agentId": "agent-001",
        "transitionKind": "fresh-origin",
        "primaryOwnerBeforeRef": {"kind": "fresh-handoff", **handoff},
        "primaryOwnerAfterRef": {
            "kind": "custody",
            "custodyRef": new_custody_ref,
            "sourceRefDigest": core.D66,
        },
        "primaryOwnerBeforeJournalDigest": core.D11,
        "primaryOwnerAfterSemanticDigest": core.D55,
        "effectsSetDigest": effects_set_digest,
        "admissionDigest": core.D33,
        "recoverySource": recovery_source,
        "sourceMode": "open-generation-loss",
        "freshHandoffDigest": core.D22,
        "freshApplyPlanDigest": core.D44,
        "affectedGenerationLossBeforeRef": before_ref,
        "affectedGenerationLossBeforeJournalDigest": core.D11,
        "affectedGenerationLossAfterRef": after_ref,
        "affectedGenerationLossAfterSemanticDigest": core.D77,
    }
    replay_digest = core.ld("transition-replay", replay)
    subject = {
        "schemaVersion": 1,
        "kind": "fresh-origin",
        "projectSessionId": "session-001",
        "runId": "run-001",
        "agentId": "agent-001",
        "ownerRef": {
            "kind": "custody",
            "custodyRef": new_custody_ref,
            "sourceRefDigest": core.D66,
        },
        "sourceMode": "open-generation-loss",
        "recoverySource": recovery_source,
        "sourceJournalDigest": core.D11,
        "admissionDigest": core.D33,
        "freshHandoffDigest": core.D22,
        "freshApplyPlanDigest": core.D44,
        "affectedGenerationLossBeforeRef": before_ref,
        "affectedGenerationLossBeforeJournalDigest": core.D11,
        "affectedGenerationLossAfterRef": after_ref,
        "affectedGenerationLossAfterSemanticDigest": core.D77,
        "freshOriginEffectDigest": effect_digest,
        "transitionReplayDigest": replay_digest,
    }
    subject_digest = core.ld("receipt-subject", subject)
    owner_digest = core.ld("receipt-owner-ref", subject["ownerRef"])
    subject_set_digest = core.ld(
        "receipt-subject-set",
        [{
            "ordinalDec": "1",
            "kind": "fresh-origin",
            "ownerRefDigest": owner_digest,
            "ownerRevisionDec": "1",
            "subjectDigest": subject_digest,
        }],
    )
    handoff_ref = {key: handoff[key] for key in HANDOFF_REF_KEYS}
    batch_body = {
        "schemaVersion": 1,
        "projectSessionId": "session-001",
        "runId": "run-001",
        "agentId": "agent-001",
        "plannedApplyId": "apply-open-001",
        "transitionKind": "fresh-origin",
        "primaryOwnerBeforeRef": {"kind": "fresh-handoff", **handoff},
        "primaryOwnerAfterRef": subject["ownerRef"],
        "primaryOwnerBeforeJournalDigest": core.D11,
        "primaryOwnerAfterSemanticDigest": core.D55,
        "effectsSetDigest": effects_set_digest,
        "transitionReplayDigest": replay_digest,
        "orderedSubjectSetDigest": subject_set_digest,
        "receiptIntentCountDec": "1",
        "secondaryIntentKind": "none",
        "reviewReservationRef": None,
        "freshHandoffRef": handoff_ref,
    }
    batch_id = core.ld("receipt-batch-id", batch_body)
    intent_body = {
        "schemaVersion": 1,
        "batchId": batch_id,
        "ordinalDec": "1",
        "kind": "fresh-origin",
        "subjectDigest": subject_digest,
        "transitionReplayDigest": replay_digest,
    }
    intent_digest = core.ld("receipt-intent", intent_body)
    authority_receipt_digest = core.fixed_digest("8")
    receipt_set_digest = core.ld(
        "authority-receipt-set",
        [{
            "ordinalDec": "1",
            "intentDigest": intent_digest,
            "authorityId": "authority-001",
            "authoritySequenceDec": "1",
            "receiptDigest": authority_receipt_digest,
            "subjectDigest": subject_digest,
        }],
    )
    completion_body = {
        "schemaVersion": 1,
        "batchId": batch_id,
        "transitionKind": "fresh-origin",
        "receiptIntentCountDec": "1",
        "secondaryIntentKind": "none",
        "ordinalOne": {
            "intentDigest": intent_digest,
            "subjectDigest": subject_digest,
            "authorityReceiptDigest": authority_receipt_digest,
        },
        "ordinalTwo": None,
        "primaryEffect": {"kind": "fresh-origin", "effectDigest": effect_digest},
        "linkedLossEffectDigest": None,
        "secondaryEffect": None,
        "orderedAuthorityReceiptSetDigest": receipt_set_digest,
    }
    completion_digest = core.ld("batch-completion", completion_body)
    apply_body = {
        "schemaVersion": 1,
        "applyKind": "fresh",
        "applyId": "apply-open-001",
        "receiptBatchId": batch_id,
        "batchCompletionDigest": completion_digest,
        "transitionReplayDigest": replay_digest,
        "orderedAuthorityReceiptSetDigest": receipt_set_digest,
        "verifiedScopeCheckpointDigest": core.D00,
        "primaryOwnerAfterRef": subject["ownerRef"],
        "freshHandoffRef": handoff_ref,
        "freshSourceMode": "open-generation-loss",
        "freshApplyPlanDigest": core.D44,
        "newCustodyRef": new_custody_ref,
        "generationLossAfterRef": after_ref,
        "freshOriginEffectDigest": effect_digest,
        "appliedMutationPlanDigest": core.D44,
        "localWriteSetDigest": core.fixed_digest("9"),
    }
    apply_digest = core.ld("transition-apply", apply_body)
    return {
        "handoff": handoff,
        "effect": effect,
        "effectDigests": [effect_digest],
        "replay": replay,
        "subject": subject,
        "batchBody": batch_body,
        "intentBody": intent_body,
        "completionBody": completion_body,
        "applyBody": apply_body,
        "goldens": {
            "effect": effect_digest,
            "effect-set": effects_set_digest,
            "replay": replay_digest,
            "subject": subject_digest,
            "subject-set": subject_set_digest,
            "batch": batch_id,
            "intent": intent_digest,
            "completion": completion_digest,
            "apply": apply_digest,
        },
    }


def terminal_fresh_vector(linked_loss: bool) -> dict[str, Any]:
    label = "linked" if linked_loss else "plain"
    old_ref = {
        "schemaVersion": 1,
        "runId": "run-001",
        "agentId": "agent-001",
        "custodyId": f"custody-terminal-{label}",
        "custodyRevision": 5,
    }
    final_ref = {**old_ref, "custodyRevision": 6}
    new_ref = {
        "schemaVersion": 1,
        "runId": "run-001",
        "agentId": "agent-001",
        "custodyId": f"custody-fresh-{label}",
        "custodyRevision": 1,
    }
    recovery_source = {"schemaVersion": 1, "kind": "custody", **old_ref}
    before_loss = ({
        "schemaVersion": 1,
        "runId": "run-001",
        "agentId": "agent-001",
        "generationLossId": f"loss-{label}",
        "generationLossRevision": 2,
    } if linked_loss else None)
    after_loss = ({**before_loss, "generationLossRevision": 3}
                  if before_loss is not None else None)
    handoff = {
        "handoffId": f"handoff-terminal-{label}",
        "handoffDigest": core.D22,
        "preparationId": f"preparation-terminal-{label}",
        "preparationDigest": core.D00,
        "plannedApplyId": f"apply-terminal-{label}",
        "sourceMode": "terminalize-nonfinal-custody",
        "recoverySource": recovery_source,
        "sourceJournalDigest": core.D11,
        "freshApplyPlanDigest": core.D44,
    }
    custody_effect = {
        "schemaVersion": 1,
        "effectKind": "owner-transition",
        "role": "primary",
        "ownerBeforeRef": {"kind": "custody", "custodyRef": old_ref},
        "beforeJournalDigest": core.D11,
        "ownerAfterRef": {"kind": "custody", "custodyRef": final_ref},
        "afterSemanticDigest": core.D55,
    }
    custody_effect_digest = core.ld("lifecycle-effect", custody_effect)
    loss_effect = None
    loss_effect_digest = None
    if linked_loss:
        loss_effect = {
            "schemaVersion": 1,
            "effectKind": "owner-transition",
            "role": "linked",
            "ownerBeforeRef": {"kind": "generation-loss", "generationLossRef": before_loss},
            "beforeJournalDigest": core.D33,
            "ownerAfterRef": {"kind": "generation-loss", "generationLossRef": after_loss},
            "afterSemanticDigest": core.D77,
        }
        loss_effect_digest = core.ld("lifecycle-effect", loss_effect)
    fresh_effect = {
        "schemaVersion": 1,
        "effectKind": "fresh-origin",
        "role": "secondary",
        "sourceMode": "terminalize-nonfinal-custody",
        "recoverySource": recovery_source,
        "sourceJournalDigest": core.D11,
        "freshHandoffDigest": core.D22,
        "admissionDigest": core.D33,
        "freshApplyPlanDigest": core.D44,
        "newCustodyRef": new_ref,
        "newCustodySemanticDigest": core.D66,
        "newCustodySourceRefDigest": core.D77,
        "affectedGenerationLossBeforeRef": before_loss,
        "affectedGenerationLossBeforeJournalDigest": core.D33 if linked_loss else None,
        "affectedGenerationLossAfterRef": after_loss,
        "affectedGenerationLossAfterSemanticDigest": core.D77 if linked_loss else None,
    }
    fresh_effect_digest = core.ld("lifecycle-effect", fresh_effect)
    effect_digests = [custody_effect_digest]
    if loss_effect_digest is not None:
        effect_digests.append(loss_effect_digest)
    effect_digests.append(fresh_effect_digest)
    effects_set_digest = core.ld("effect-set", effect_digests)
    replay = {
        "schemaVersion": 1,
        "transactionId": f"apply-terminal-{label}",
        "projectSessionId": "session-001",
        "runId": "run-001",
        "agentId": "agent-001",
        "transitionKind": "custody-terminal",
        "primaryOwnerBeforeRef": {"kind": "custody", "custodyRef": old_ref},
        "primaryOwnerAfterRef": {"kind": "custody", "custodyRef": final_ref},
        "primaryOwnerBeforeJournalDigest": core.D11,
        "primaryOwnerAfterSemanticDigest": core.D55,
        "effectsSetDigest": effects_set_digest,
        "admissionDigest": core.D33,
        "transitionProofDigest": core.D00,
        "mutationPlanDigest": core.D44,
        "affectedGenerationLossBeforeRef": before_loss,
        "affectedGenerationLossBeforeJournalDigest": core.D33 if linked_loss else None,
        "affectedGenerationLossAfterRef": after_loss,
        "affectedGenerationLossAfterSemanticDigest": core.D77 if linked_loss else None,
    }
    replay_digest = core.ld("transition-replay", replay)
    custody_subject = {
        "schemaVersion": 1,
        "kind": "custody-terminal",
        "projectSessionId": "session-001",
        "runId": "run-001",
        "agentId": "agent-001",
        "ownerRef": {"kind": "custody", "custodyRef": final_ref, "sourceRefDigest": core.D66},
        "custodyEffectDigest": custody_effect_digest,
        "transitionReplayDigest": replay_digest,
    }
    fresh_subject = {
        "schemaVersion": 1,
        "kind": "fresh-origin",
        "projectSessionId": "session-001",
        "runId": "run-001",
        "agentId": "agent-001",
        "ownerRef": {"kind": "custody", "custodyRef": new_ref, "sourceRefDigest": core.D77},
        "sourceMode": "terminalize-nonfinal-custody",
        "recoverySource": recovery_source,
        "sourceJournalDigest": core.D11,
        "admissionDigest": core.D33,
        "freshHandoffDigest": core.D22,
        "freshApplyPlanDigest": core.D44,
        "affectedGenerationLossBeforeRef": before_loss,
        "affectedGenerationLossBeforeJournalDigest": core.D33 if linked_loss else None,
        "affectedGenerationLossAfterRef": after_loss,
        "affectedGenerationLossAfterSemanticDigest": core.D77 if linked_loss else None,
        "freshOriginEffectDigest": fresh_effect_digest,
        "transitionReplayDigest": replay_digest,
    }
    custody_subject_digest = core.ld("receipt-subject", custody_subject)
    fresh_subject_digest = core.ld("receipt-subject", fresh_subject)
    subject_members = [
        {
            "ordinalDec": "1",
            "kind": "custody-terminal",
            "ownerRefDigest": core.ld("receipt-owner-ref", custody_subject["ownerRef"]),
            "ownerRevisionDec": "6",
            "subjectDigest": custody_subject_digest,
        },
        {
            "ordinalDec": "2",
            "kind": "fresh-origin",
            "ownerRefDigest": core.ld("receipt-owner-ref", fresh_subject["ownerRef"]),
            "ownerRevisionDec": "1",
            "subjectDigest": fresh_subject_digest,
        },
    ]
    subject_set_digest = core.ld("receipt-subject-set", subject_members)
    handoff_ref = {key: handoff[key] for key in HANDOFF_REF_KEYS}
    batch_body = {
        "schemaVersion": 1,
        "projectSessionId": "session-001",
        "runId": "run-001",
        "agentId": "agent-001",
        "plannedApplyId": f"apply-terminal-{label}",
        "transitionKind": "custody-terminal",
        "primaryOwnerBeforeRef": replay["primaryOwnerBeforeRef"],
        "primaryOwnerAfterRef": replay["primaryOwnerAfterRef"],
        "primaryOwnerBeforeJournalDigest": core.D11,
        "primaryOwnerAfterSemanticDigest": core.D55,
        "effectsSetDigest": effects_set_digest,
        "transitionReplayDigest": replay_digest,
        "orderedSubjectSetDigest": subject_set_digest,
        "receiptIntentCountDec": "2",
        "secondaryIntentKind": "fresh-origin",
        "reviewReservationRef": None,
        "freshHandoffRef": handoff_ref,
    }
    batch_id = core.ld("receipt-batch-id", batch_body)
    intent_bodies = [
        {
            "schemaVersion": 1,
            "batchId": batch_id,
            "ordinalDec": str(index),
            "kind": subject["kind"],
            "subjectDigest": digest,
            "transitionReplayDigest": replay_digest,
        }
        for index, (subject, digest) in enumerate(
            ((custody_subject, custody_subject_digest),
             (fresh_subject, fresh_subject_digest)),
            start=1,
        )
    ]
    intent_digests = [core.ld("receipt-intent", body) for body in intent_bodies]
    receipt_digests = [core.fixed_digest("8"), core.fixed_digest("9")]
    receipt_set_digest = core.ld(
        "authority-receipt-set",
        [
            {
                "ordinalDec": str(index),
                "intentDigest": intent_digests[index - 1],
                "authorityId": "authority-001",
                "authoritySequenceDec": str(index),
                "receiptDigest": receipt_digests[index - 1],
                "subjectDigest": (custody_subject_digest, fresh_subject_digest)[index - 1],
            }
            for index in (1, 2)
        ],
    )
    completion_body = {
        "schemaVersion": 1,
        "batchId": batch_id,
        "transitionKind": "custody-terminal",
        "receiptIntentCountDec": "2",
        "secondaryIntentKind": "fresh-origin",
        "ordinalOne": {
            "intentDigest": intent_digests[0],
            "subjectDigest": custody_subject_digest,
            "authorityReceiptDigest": receipt_digests[0],
        },
        "ordinalTwo": {
            "intentDigest": intent_digests[1],
            "subjectDigest": fresh_subject_digest,
            "authorityReceiptDigest": receipt_digests[1],
        },
        "primaryEffect": {"kind": "custody", "effectDigest": custody_effect_digest},
        "linkedLossEffectDigest": loss_effect_digest,
        "secondaryEffect": {"kind": "fresh-origin", "effectDigest": fresh_effect_digest},
        "orderedAuthorityReceiptSetDigest": receipt_set_digest,
    }
    completion_digest = core.ld("batch-completion", completion_body)
    apply_body = {
        "schemaVersion": 1,
        "applyKind": "terminal-fresh",
        "applyId": f"apply-terminal-{label}",
        "receiptBatchId": batch_id,
        "batchCompletionDigest": completion_digest,
        "transitionReplayDigest": replay_digest,
        "orderedAuthorityReceiptSetDigest": receipt_set_digest,
        "verifiedScopeCheckpointDigest": core.D00,
        "primaryOwnerAfterRef": replay["primaryOwnerAfterRef"],
        "freshHandoffRef": handoff_ref,
        "freshSourceMode": "terminalize-nonfinal-custody",
        "freshApplyPlanDigest": core.D44,
        "newCustodyRef": new_ref,
        "generationLossAfterRef": after_loss,
        "freshOriginEffectDigest": fresh_effect_digest,
        "appliedMutationPlanDigest": core.D44,
        "localWriteSetDigest": core.fixed_digest("a"),
    }
    apply_digest = core.ld("transition-apply", apply_body)
    return {
        "handoff": handoff,
        "effects": [custody_effect, *([loss_effect] if loss_effect else []), fresh_effect],
        "effectDigests": effect_digests,
        "replay": replay,
        "subjects": [custody_subject, fresh_subject],
        "batchBody": batch_body,
        "intentBodies": intent_bodies,
        "completionBody": completion_body,
        "applyBody": apply_body,
        "goldens": {
            "effect-set": effects_set_digest,
            "replay": replay_digest,
            "custody-subject": custody_subject_digest,
            "fresh-subject": fresh_subject_digest,
            "subject-set": subject_set_digest,
            "batch": batch_id,
            "intent-1": intent_digests[0],
            "intent-2": intent_digests[1],
            "completion": completion_digest,
            "apply": apply_digest,
        },
    }


def validate_pure_fresh_bundle(vector: dict[str, Any], expected: dict[str, Any]) -> None:
    batch = vector["batchBody"]
    before = exact_object(batch["primaryOwnerBeforeRef"], BEFORE_REF_KEYS, "fresh authority ref")
    handoff_ref = exact_object(batch["freshHandoffRef"], HANDOFF_REF_KEYS, "freshHandoffRef")
    expected_before = expected["batchBody"]["primaryOwnerBeforeRef"]
    require(before == expected_before, "crossed primary owner before ref")
    require(handoff_ref == expected["batchBody"]["freshHandoffRef"], "crossed handoff ref")
    require(before["kind"] == "fresh-handoff", "fresh before arm kind")
    for key in HANDOFF_REF_KEYS:
        require(handoff_ref[key] == before[key], f"handoff subset crossed: {key}")
    require(batch["plannedApplyId"] == before["plannedApplyId"], "crossed planned apply")
    replay = vector["replay"]
    effect = vector["effect"]
    subject = vector["subject"]
    for value, label in ((replay, "replay"), (effect, "effect"), (subject, "subject")):
        require(value["recoverySource"] == before["recoverySource"], f"crossed {label} source")
        journal_key = ("primaryOwnerBeforeJournalDigest"
                       if label == "replay" else "sourceJournalDigest")
        require(value[journal_key] == before["sourceJournalDigest"], f"crossed {label} journal")
        require(value["freshHandoffDigest"] == before["handoffDigest"], f"crossed {label} handoff")
        require(value["freshApplyPlanDigest"] == before["freshApplyPlanDigest"], f"crossed {label} plan")
    require(replay["primaryOwnerBeforeRef"] == before, "replay before ref crossed")
    require(batch["primaryOwnerBeforeJournalDigest"] == before["sourceJournalDigest"],
            "batch before journal crossed")
    require(batch["effectsSetDigest"] == core.ld("effect-set", vector["effectDigests"]),
            "crossed effect set")
    require(subject["freshOriginEffectDigest"] == vector["effectDigests"][0],
            "crossed subject effect")
    new_ref = subject["ownerRef"]["custodyRef"]
    require(new_ref["custodyRevision"] == 1, "fresh custody revision is not one")
    if subject["sourceMode"] == "open-generation-loss":
        before_loss = subject["affectedGenerationLossBeforeRef"]
        after_loss = subject["affectedGenerationLossAfterRef"]
        source = subject["recoverySource"]
        require(before_loss["generationLossId"] == source["generationLossId"],
                "open loss source identity crossed")
        require(before_loss["generationLossRevision"] == source["generationLossRevision"],
                "open loss source revision crossed")
        require(after_loss["generationLossId"] == before_loss["generationLossId"],
                "open loss after identity crossed")
        require(after_loss["generationLossRevision"] == before_loss["generationLossRevision"] + 1,
                "open loss after revision is not plus one")
    require(core.ld("receipt-batch-id", batch) == vector["goldens"]["batch"],
            "batch ID/body mismatch")


def validate_generic_terminal_replay(replay: dict[str, Any]) -> None:
    require(replay.get("transitionKind") in (
        "custody-terminal", "generation-loss-terminal", "custody-recovery-retirement"
    ), "fresh replay parsed as generic terminal replay")


OPEN_GOLDENS = {
    "effect": "sha256:fb401b07197b9bbcc7d4796ce9521897aff7ebdf6f30d5e3c72383e8b8f3c6e5",
    "effect-set": "sha256:dee9509ad81d7d22d141d713631fb871ea0b9d91791de41d2c09ec7cc1aba177",
    "replay": "sha256:b6194fe6b4c2fca09a006b886a5c0c82a4eb74780e9e23773d8e17ca4335e1e2",
    "subject": "sha256:22a38163ef0317849b21da11ce9006b4e35744782ec1da97ed6ebd91c7c0275b",
    "subject-set": "sha256:8193b4bbe32c4e950b08bca630980b8ca7f66035664d495bcd72a0555a811658",
    "batch": "sha256:d77e8718eab56957c2faf453315e28725d78574e49da5cdaadc26407ed28434e",
    "intent": "sha256:f1e5265c3bf27e75decd1ee416b578d1712a25126fd35124adbb45a360ca9fad",
    "completion": "sha256:cb2ae642b8878c72c10eb9233095804fa96449e0e980812a51481f679fc628df",
    "apply": "sha256:26bf81ed9dfc74c04416ed7419bdcaeb87220dbcd6f2e77bac8645d2e717f9dd",
}
TERMINAL_PLAIN_GOLDENS = {
    "effect-set": "sha256:1de4abdd113c465138c8560f6bc3b1b19ae5fa23414c81aceb519fcf66bbbdcf",
    "replay": "sha256:1497c25c9b540105ead71a0031fc6e1b406eaee2aacbbb7358776d3b6d4d59a1",
    "custody-subject": "sha256:537a16054dd92d69f087f8fcee7353150a34b494a46114b5eaf131372c1ca1d7",
    "fresh-subject": "sha256:86976a6896284a2d89f8db81f608ba6344ef35d3a5dbabf3e6510cbbb32848db",
    "subject-set": "sha256:504191a24ff3e633255a037581394978481466989d3fed742b76fff3e5a8451a",
    "batch": "sha256:ef5b225f453fd1ea6e64f9fc63de9871b7ff1059d37f267569c799d88ab4ce96",
    "intent-1": "sha256:e56045b2468cf67bb041a7a6adc9dc3d18379379765b0fb21e2beac8913ba8ea",
    "intent-2": "sha256:3f516ad31a669e24bcb9d9deaa80d5812f9725d915bb011b776166ede8ff3e3d",
    "completion": "sha256:fba156a34e448198dc121d937ef19e882fb02c3b77331f13aab432cb063ede81",
    "apply": "sha256:0b1258baa2b75b4a26a37d190fa0eab0c0cd90693e3793cc52d00aaa7890de40",
}
TERMINAL_LINKED_GOLDENS = {
    "effect-set": "sha256:7068dd68a0de1e37f6b49c555edb0bd07a8f46d236a801c56a4485bbf2cc8f4a",
    "replay": "sha256:0cf9d5c303b66d38efccf04682b6fe26c829e1b3f4aa13de8ebc5f249a27682e",
    "custody-subject": "sha256:73b9d6ef673f775db3de50156051ba06a8da9b3a0ecbeeaa272821cf64c4f10f",
    "fresh-subject": "sha256:521c812b6116e603ff7154095466dcac5b2ec42c04689ea69ae1d7cebf169729",
    "subject-set": "sha256:c5907e996362b93a5ed7bda77b1b336af2c41e44957e487036263a4d1fb1b877",
    "batch": "sha256:39abd2d835dc370f8e8e8b83421527b4ded3dfeae8462e7fa03b13f6d4c86fdc",
    "intent-1": "sha256:3616d228b59c10d164b9219b4c8facb24758ffdd8e834d59e6d3c90bef1a65e1",
    "intent-2": "sha256:7234a091b1bf25d996051d5f8cc95b5c9ea9e3486eb29f9f8ccaafdcc24407a5",
    "completion": "sha256:c48cc6580937cdbc347a5bca40ca37a82f79e05e2cfc01fb6e49c6089f69f489",
    "apply": "sha256:0744e066aa75a4ff1ce3888cd06ac6edadd9738b1818e4a0dea7779cd8a9dfee",
}


@case("FO-C02")
def open_loss_codec_and_goldens_are_exact() -> None:
    vector = open_loss_vector()
    validate_pure_fresh_bundle(vector, vector)
    require(vector["goldens"] == OPEN_GOLDENS, f"open goldens: {vector['goldens']}")


@case("FO-C03")
def terminal_fresh_goldens_cover_both_effect_counts() -> None:
    plain = terminal_fresh_vector(False)
    linked = terminal_fresh_vector(True)
    require(len(plain["effectDigests"]) == 2, "plain terminal-fresh effect count")
    require(len(linked["effectDigests"]) == 3, "linked terminal-fresh effect count")
    for vector in (plain, linked):
        require(len(vector["intentBodies"]) == 2, "terminal-fresh intent count")
        require(vector["batchBody"]["receiptIntentCountDec"] == "2", "batch count")
        require(vector["batchBody"]["secondaryIntentKind"] == "fresh-origin", "ordinal two")
    require(plain["goldens"] == TERMINAL_PLAIN_GOLDENS,
            f"plain terminal goldens: {plain['goldens']}")
    require(linked["goldens"] == TERMINAL_LINKED_GOLDENS,
            f"linked terminal goldens: {linked['goldens']}")


@case("FO-C04")
def every_pure_fresh_before_ref_member_is_closed_and_bound() -> None:
    expected = open_loss_vector()
    for key in BEFORE_REF_KEYS:
        omitted = deepcopy(expected)
        omitted["batchBody"]["primaryOwnerBeforeRef"].pop(key)
        try:
            validate_pure_fresh_bundle(omitted, expected)
        except (core.CodecError, core.OracleFailure):
            pass
        else:
            raise core.OracleFailure(f"omitted before-ref member accepted: {key}")

        mutant = deepcopy(expected)
        mutant["batchBody"]["primaryOwnerBeforeRef"][key] = changed(
            mutant["batchBody"]["primaryOwnerBeforeRef"][key]
        )
        try:
            validate_pure_fresh_bundle(mutant, expected)
        except (core.CodecError, core.OracleFailure):
            pass
        else:
            raise core.OracleFailure(f"mutated before-ref member accepted: {key}")

    try:
        validate_generic_terminal_replay(expected["replay"])
    except core.OracleFailure:
        pass
    else:
        raise core.OracleFailure("fresh replay used a terminal-shaped fallback")


@case("FO-C05")
def crossed_handoff_plan_source_effect_and_revision_reject() -> None:
    expected = open_loss_vector()
    mutations: tuple[tuple[str, Callable[[dict[str, Any]], None]], ...] = (
        ("handoff", lambda value: value["batchBody"]["freshHandoffRef"].__setitem__("handoffDigest", core.D77)),
        ("preparation", lambda value: value["batchBody"]["freshHandoffRef"].__setitem__("preparationId", "crossed-preparation")),
        ("journal", lambda value: value["replay"].__setitem__("primaryOwnerBeforeJournalDigest", core.D77)),
        ("plan", lambda value: value["effect"].__setitem__("freshApplyPlanDigest", core.D77)),
        ("effect", lambda value: value["subject"].__setitem__("freshOriginEffectDigest", core.D00)),
        ("revision-one", lambda value: value["subject"]["ownerRef"]["custodyRef"].__setitem__("custodyRevision", 2)),
        ("before-source", lambda value: value["subject"]["affectedGenerationLossBeforeRef"].__setitem__("generationLossRevision", 4)),
        ("after-identity", lambda value: value["subject"]["affectedGenerationLossAfterRef"].__setitem__("generationLossId", "loss-crossed")),
        ("after-revision", lambda value: value["subject"]["affectedGenerationLossAfterRef"].__setitem__("generationLossRevision", 9)),
        ("effect-set", lambda value: value["batchBody"].__setitem__("effectsSetDigest", core.D00)),
    )
    for label, mutate in mutations:
        mutant = deepcopy(expected)
        mutate(mutant)
        try:
            validate_pure_fresh_bundle(mutant, expected)
        except core.OracleFailure:
            pass
        else:
            raise core.OracleFailure(f"crossed fresh bundle accepted: {label}")


@case("FO-C06")
def batch_id_key_reuse_conflicts_after_member_mutation() -> None:
    vector = open_loss_vector()
    connection = sqlite3.connect(":memory:", isolation_level=None)
    connection.execute(
        "CREATE TABLE batch_ids(batch_id TEXT PRIMARY KEY, body_json TEXT NOT NULL UNIQUE)"
    )
    body_json = core.jcs(vector["batchBody"]).decode("utf-8")
    connection.execute("INSERT INTO batch_ids VALUES(?,?)", (vector["goldens"]["batch"], body_json))
    for key in core.BATCH_BODY_KEYS:
        mutant = deepcopy(vector["batchBody"])
        mutant[key] = changed(mutant[key])
        mutant_id = core.ld("receipt-batch-id", mutant)
        require(mutant_id != vector["goldens"]["batch"], f"unauthenticated body member: {key}")
    crossed = deepcopy(vector["batchBody"])
    crossed["secondaryIntentKind"] = "review-adoption-decision"
    core.expect_integrity(
        lambda: connection.execute(
            "INSERT INTO batch_ids VALUES(?,?)",
            (vector["goldens"]["batch"], core.jcs(crossed).decode("utf-8")),
        ),
        "UNIQUE constraint failed: batch_ids.batch_id",
    )
    require(core.ld("receipt-batch-id", dict(reversed(list(vector["batchBody"].items()))))
            == vector["goldens"]["batch"], "JCS key order changed batch ID")
    connection.close()


APPLY_EXTENSION_SQL = r"""
PRAGMA foreign_keys=ON;

CREATE TRIGGER fresh_effect_requires_same_batch_handoff
BEFORE INSERT ON fresh_origin_effects
WHEN NOT EXISTS (
  SELECT 1 FROM receipt_batches AS b
  WHERE b.batch_id=NEW.batch_id
    AND b.planned_apply_id=NEW.planned_apply_id
    AND b.fresh_handoff_id=NEW.handoff_id
    AND b.fresh_handoff_digest=NEW.handoff_digest
    AND b.fresh_handoff_source_mode=NEW.handoff_source_mode
)
BEGIN
  SELECT RAISE(ABORT,'fresh-effect-batch-handoff-crossed');
END;

CREATE TABLE nonfresh_effects(
  batch_id TEXT NOT NULL,
  effect_order INTEGER NOT NULL CHECK(effect_order >= 1),
  effect_kind TEXT NOT NULL CHECK(effect_kind IN ('custody','generation-loss')),
  effect_role TEXT NOT NULL CHECK(effect_role IN ('primary','linked')),
  effect_digest TEXT NOT NULL UNIQUE,
  PRIMARY KEY(batch_id,effect_order),
  FOREIGN KEY(batch_id) REFERENCES receipt_batches(batch_id)
);

CREATE TABLE batch_authorizations(
  batch_id TEXT PRIMARY KEY,
  completion_digest TEXT NOT NULL,
  authorization_digest TEXT NOT NULL UNIQUE,
  UNIQUE(batch_id,completion_digest,authorization_digest),
  FOREIGN KEY(batch_id) REFERENCES batch_completions(batch_id),
  FOREIGN KEY(completion_digest) REFERENCES batch_completions(completion_digest)
);

CREATE TRIGGER authorization_requires_same_completion
BEFORE INSERT ON batch_authorizations
WHEN NOT EXISTS (
  SELECT 1 FROM batch_completions AS c
  WHERE c.batch_id=NEW.batch_id
    AND c.completion_digest=NEW.completion_digest
)
BEGIN
  SELECT RAISE(ABORT,'authorization-completion-crossed');
END;

CREATE TABLE apply_plans(
  apply_id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL UNIQUE,
  apply_kind TEXT NOT NULL CHECK(apply_kind IN ('fresh','terminal-fresh')),
  source_mode TEXT NOT NULL CHECK(source_mode IN
    ('reuse-final-custody','open-generation-loss','terminalize-nonfinal-custody')),
  fresh_effect_ordinal INTEGER NOT NULL CHECK(fresh_effect_ordinal IN (1,2)),
  fresh_effect_role TEXT NOT NULL CHECK(fresh_effect_role IN ('primary','secondary')),
  fresh_effect_digest TEXT NOT NULL,
  expected_write_count INTEGER NOT NULL CHECK(expected_write_count BETWEEN 2 AND 4),
  UNIQUE(apply_id,batch_id,apply_kind,source_mode,fresh_effect_ordinal,
    fresh_effect_role,fresh_effect_digest,expected_write_count),
  FOREIGN KEY(batch_id) REFERENCES receipt_batches(batch_id),
  FOREIGN KEY(batch_id,fresh_effect_ordinal,fresh_effect_role,fresh_effect_digest)
    REFERENCES fresh_origin_effects(
      batch_id,receipt_ordinal,effect_role,effect_digest),
  CHECK((apply_kind='fresh' AND source_mode IN
      ('reuse-final-custody','open-generation-loss') AND
      fresh_effect_ordinal=1 AND fresh_effect_role='primary') OR
    (apply_kind='terminal-fresh' AND
      source_mode='terminalize-nonfinal-custody' AND
      fresh_effect_ordinal=2 AND fresh_effect_role='secondary'))
);

CREATE TABLE planned_writes(
  apply_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 1),
  relation_name TEXT NOT NULL CHECK(relation_name IN
    ('source-custody','custody','generation-loss','commit')),
  row_key TEXT NOT NULL,
  row_revision INTEGER NOT NULL CHECK(row_revision >= 1),
  post_digest TEXT NOT NULL,
  PRIMARY KEY(apply_id,ordinal),
  UNIQUE(apply_id,relation_name,row_key),
  UNIQUE(apply_id,ordinal,relation_name,row_key,row_revision,post_digest),
  FOREIGN KEY(apply_id) REFERENCES apply_plans(apply_id)
);

CREATE TABLE semantic_writes(
  apply_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  relation_name TEXT NOT NULL,
  row_key TEXT NOT NULL,
  row_revision INTEGER NOT NULL,
  post_digest TEXT NOT NULL,
  PRIMARY KEY(apply_id,ordinal),
  UNIQUE(apply_id,relation_name,row_key),
  FOREIGN KEY(apply_id,ordinal,relation_name,row_key,row_revision,post_digest)
    REFERENCES planned_writes(
      apply_id,ordinal,relation_name,row_key,row_revision,post_digest),
  FOREIGN KEY(apply_id) REFERENCES transition_applies(apply_id)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE transition_applies(
  apply_id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL UNIQUE,
  completion_digest TEXT NOT NULL,
  authorization_digest TEXT NOT NULL,
  apply_kind TEXT NOT NULL,
  source_mode TEXT NOT NULL,
  fresh_effect_ordinal INTEGER NOT NULL,
  fresh_effect_role TEXT NOT NULL,
  fresh_effect_digest TEXT NOT NULL,
  expected_write_count INTEGER NOT NULL,
  apply_digest TEXT NOT NULL UNIQUE,
  UNIQUE(apply_id,batch_id,apply_kind,source_mode,fresh_effect_ordinal,
    fresh_effect_role,fresh_effect_digest,expected_write_count),
  FOREIGN KEY(batch_id,completion_digest,authorization_digest)
    REFERENCES batch_authorizations(
      batch_id,completion_digest,authorization_digest),
  FOREIGN KEY(apply_id,batch_id,apply_kind,source_mode,fresh_effect_ordinal,
      fresh_effect_role,fresh_effect_digest,expected_write_count)
    REFERENCES apply_plans(
      apply_id,batch_id,apply_kind,source_mode,fresh_effect_ordinal,
      fresh_effect_role,fresh_effect_digest,expected_write_count),
  FOREIGN KEY(batch_id,fresh_effect_ordinal,fresh_effect_role,fresh_effect_digest)
    REFERENCES fresh_origin_effects(
      batch_id,receipt_ordinal,effect_role,effect_digest)
);

CREATE TRIGGER apply_marker_must_be_last
BEFORE INSERT ON transition_applies
WHEN (SELECT COUNT(*) FROM semantic_writes AS w
      WHERE w.apply_id=NEW.apply_id) <> NEW.expected_write_count
  OR EXISTS (
    SELECT 1 FROM planned_writes AS p
    WHERE p.apply_id=NEW.apply_id
      AND NOT EXISTS (
        SELECT 1 FROM semantic_writes AS w
        WHERE w.apply_id=p.apply_id AND w.ordinal=p.ordinal
          AND w.relation_name=p.relation_name AND w.row_key=p.row_key
          AND w.row_revision=p.row_revision AND w.post_digest=p.post_digest
      )
  )
BEGIN
  SELECT RAISE(ABORT,'apply-children-incomplete');
END;
"""


def apply_db() -> sqlite3.Connection:
    connection = core.relational_db()
    connection.executescript(APPLY_EXTENSION_SQL)
    return connection


def prepare_fresh_apply(
    connection: sqlite3.Connection,
    suffix: str,
    source_mode: str,
    *,
    linked_loss: bool = False,
    authorize: bool = True,
) -> dict[str, Any]:
    terminal = source_mode == "terminalize-nonfinal-custody"
    transition = "custody-terminal" if terminal else "fresh-origin"
    secondary = "fresh-origin" if terminal else "none"
    count = 2 if terminal else 1
    batch = core.insert_batch(
        connection,
        suffix,
        transition,
        secondary,
        count,
        False,
        source_mode,
        False,
    )
    ordinal = 2 if terminal else 1
    role = "secondary" if terminal else "primary"
    effect_digest = core.ld(
        "lifecycle-effect",
        {"schemaVersion": 1, "batchId": batch, "role": role, "sourceMode": source_mode},
    )
    connection.execute(
        "INSERT INTO fresh_origin_effects VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        (
            batch,
            ordinal,
            transition,
            count,
            secondary,
            f"handoff-{suffix}",
            core.D22,
            f"apply-{suffix}",
            source_mode,
            role,
            effect_digest,
        ),
    )
    if terminal:
        connection.execute(
            "INSERT INTO nonfresh_effects VALUES(?,?,?,?,?)",
            (batch, 1, "custody", "primary", core.ld("lifecycle-effect", {"batch": batch, "kind": "custody"})),
        )
        if linked_loss:
            connection.execute(
                "INSERT INTO nonfresh_effects VALUES(?,?,?,?,?)",
                (batch, 2, "generation-loss", "linked", core.ld("lifecycle-effect", {"batch": batch, "kind": "loss"})),
            )
    first = core.insert_intent_and_receipt(
        connection, batch, 1, transition, transition, count, secondary
    )
    second = (
        core.insert_intent_and_receipt(
            connection, batch, 2, "fresh-origin", transition, count, secondary
        )
        if terminal else None
    )
    core.insert_completion(
        connection, batch, transition, count, secondary, first, second, effect_digest
    )
    if terminal and linked_loss:
        linked_digest = connection.execute(
            "SELECT effect_digest FROM nonfresh_effects WHERE batch_id=? AND effect_role='linked'",
            (batch,),
        ).fetchone()[0]
        connection.execute(
            "UPDATE batch_completions SET linked_loss_effect_digest=? WHERE batch_id=?",
            (linked_digest, batch),
        )
    completion_digest = connection.execute(
        "SELECT completion_digest FROM batch_completions WHERE batch_id=?", (batch,)
    ).fetchone()[0]
    authorization_digest = core.ld(
        "batch-authorization",
        {"schemaVersion": 1, "batchId": batch, "completionDigest": completion_digest},
    )
    if authorize:
        connection.execute(
            "INSERT INTO batch_authorizations VALUES(?,?,?)",
            (batch, completion_digest, authorization_digest),
        )
    if terminal:
        writes = [
            ("source-custody", f"old-{suffix}", 6, core.fixed_digest("b")),
            ("custody", f"new-{suffix}", 1, core.fixed_digest("c")),
        ]
        if linked_loss:
            writes.append(("generation-loss", f"loss-{suffix}", 3, core.fixed_digest("d")))
        writes.append(("commit", f"commit-{suffix}", 1, core.fixed_digest("e")))
        apply_kind = "terminal-fresh"
    else:
        writes = [("custody", f"new-{suffix}", 1, core.fixed_digest("c"))]
        if source_mode == "open-generation-loss":
            writes.append(("generation-loss", f"loss-{suffix}", 4, core.fixed_digest("d")))
        writes.append(("commit", f"commit-{suffix}", 1, core.fixed_digest("e")))
        apply_kind = "fresh"
    apply_id = f"apply-{suffix}"
    connection.execute(
        "INSERT INTO apply_plans VALUES(?,?,?,?,?,?,?,?)",
        (apply_id, batch, apply_kind, source_mode, ordinal, role, effect_digest, len(writes)),
    )
    for index, (relation, row_key, revision, digest) in enumerate(writes, start=1):
        connection.execute(
            "INSERT INTO planned_writes VALUES(?,?,?,?,?,?)",
            (apply_id, index, relation, row_key, revision, digest),
        )
    return {
        "applyId": apply_id,
        "batchId": batch,
        "completionDigest": completion_digest,
        "authorizationDigest": authorization_digest,
        "applyKind": apply_kind,
        "sourceMode": source_mode,
        "effectOrdinal": ordinal,
        "effectRole": role,
        "effectDigest": effect_digest,
        "writeCount": len(writes),
    }


def insert_apply_marker(connection: sqlite3.Connection, prepared: dict[str, Any]) -> None:
    apply_digest = core.ld(
        "transition-apply",
        {
            "schemaVersion": 1,
            "applyId": prepared["applyId"],
            "batchId": prepared["batchId"],
            "completionDigest": prepared["completionDigest"],
            "authorizationDigest": prepared["authorizationDigest"],
        },
    )
    connection.execute(
        "INSERT INTO transition_applies VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        (
            prepared["applyId"],
            prepared["batchId"],
            prepared["completionDigest"],
            prepared["authorizationDigest"],
            prepared["applyKind"],
            prepared["sourceMode"],
            prepared["effectOrdinal"],
            prepared["effectRole"],
            prepared["effectDigest"],
            prepared["writeCount"],
            apply_digest,
        ),
    )


def apply_prepared(
    connection: sqlite3.Connection,
    prepared: dict[str, Any],
    fault_before: int | None = None,
) -> None:
    existing = connection.execute(
        "SELECT apply_id FROM transition_applies WHERE apply_id=?", (prepared["applyId"],)
    ).fetchone()
    if existing is not None:
        actual = connection.execute(
            "SELECT ordinal,relation_name,row_key,row_revision,post_digest "
            "FROM semantic_writes WHERE apply_id=? ORDER BY ordinal",
            (prepared["applyId"],),
        ).fetchall()
        planned = connection.execute(
            "SELECT ordinal,relation_name,row_key,row_revision,post_digest "
            "FROM planned_writes WHERE apply_id=? ORDER BY ordinal",
            (prepared["applyId"],),
        ).fetchall()
        require(actual == planned, "applied post-state replay is crossed")
        return
    writes = connection.execute(
        "SELECT ordinal,relation_name,row_key,row_revision,post_digest "
        "FROM planned_writes WHERE apply_id=? ORDER BY ordinal",
        (prepared["applyId"],),
    ).fetchall()
    connection.execute("BEGIN IMMEDIATE")
    try:
        for boundary, row in enumerate(writes, start=1):
            if fault_before == boundary:
                raise core.SimulatedCrash(f"before-apply-child-{boundary}")
            connection.execute(
                "INSERT INTO semantic_writes VALUES(?,?,?,?,?,?)",
                (prepared["applyId"], *row),
            )
        if fault_before == len(writes) + 1:
            raise core.SimulatedCrash("before-apply-marker")
        insert_apply_marker(connection, prepared)
        connection.commit()
    except Exception:
        connection.rollback()
        raise


def fresh_post_counts(connection: sqlite3.Connection, prepared: dict[str, Any]) -> dict[str, int]:
    apply_id = prepared["applyId"]
    result = {
        "writes": connection.execute(
            "SELECT COUNT(*) FROM semantic_writes WHERE apply_id=?", (apply_id,)
        ).fetchone()[0],
        "applies": connection.execute(
            "SELECT COUNT(*) FROM transition_applies WHERE apply_id=?", (apply_id,)
        ).fetchone()[0],
    }
    for relation in ("source-custody", "custody", "generation-loss", "commit"):
        result[relation] = connection.execute(
            "SELECT COUNT(*) FROM semantic_writes WHERE apply_id=? AND relation_name=?",
            (apply_id, relation),
        ).fetchone()[0]
    return result


@case("FO-S05")
def fresh_completion_rejects_absent_wrong_or_crossed_effect() -> None:
    connection = apply_db()
    batch = core.insert_batch(
        connection, "s05-absent", "fresh-origin", "none", 1, False,
        "reuse-final-custody", False
    )
    first = core.insert_intent_and_receipt(
        connection, batch, 1, "fresh-origin", "fresh-origin", 1, "none"
    )

    def completion_without_effect(db: sqlite3.Connection) -> None:
        core.insert_completion(
            db,
            batch,
            "fresh-origin",
            1,
            "none",
            first,
            None,
            core.D66,
        )

    def completion_with_effect(db: sqlite3.Connection) -> None:
        db.execute(
            "INSERT INTO fresh_origin_effects VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            (
                batch,
                1,
                "fresh-origin",
                1,
                "none",
                "handoff-s05-absent",
                core.D22,
                "apply-s05-absent",
                "reuse-final-custody",
                "primary",
                core.D66,
            ),
        )
        completion_without_effect(db)

    completion_effect_fk = ForeignKeySpec(
        "batch_completions",
        (
            "batch_id",
            "primary_fresh_ordinal",
            "primary_fresh_role",
            "primary_fresh_digest",
        ),
        "fresh_origin_effects",
        ("batch_id", "receipt_ordinal", "effect_role", "effect_digest"),
    )
    assert_fk_rejected(
        connection,
        invalid_operation=completion_without_effect,
        positive_control=completion_with_effect,
        expected=frozenset({completion_effect_fk}),
    )
    connection.close()

    for ordinal, role in ((2, "primary"), (1, "secondary")):
        connection = apply_db()
        batch = core.insert_batch(
            connection, f"s05-{ordinal}-{role}", "fresh-origin", "none", 1,
            False, "reuse-final-custody", False
        )
        core.expect_integrity(
            lambda ordinal=ordinal, role=role, batch=batch: connection.execute(
                "INSERT INTO fresh_origin_effects VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                (batch, ordinal, "fresh-origin", 1, "none",
                 f"handoff-s05-{ordinal}-{role}", core.D22,
                 f"apply-s05-{ordinal}-{role}", "reuse-final-custody", role, core.D66),
            ),
            """CHECK constraint failed: (batch_transition_kind='fresh-origin' AND receipt_ordinal=1 AND
      effect_role='primary') OR
    (batch_transition_kind='custody-terminal' AND receipt_ordinal=2 AND
      effect_role='secondary')""",
        )
        connection.close()

    connection = apply_db()
    first_prepared = prepare_fresh_apply(
        connection, "s05-a", "reuse-final-custody", authorize=False
    )
    second_prepared = prepare_fresh_apply(
        connection, "s05-b", "reuse-final-custody", authorize=False
    )
    connection.execute("DELETE FROM batch_completions WHERE batch_id=?", (first_prepared["batchId"],))
    first_receipt = connection.execute(
        "SELECT intent_digest,subject_digest,receipt_digest FROM authority_receipts "
        "WHERE batch_id=? AND ordinal=1", (first_prepared["batchId"],)
    ).fetchone()
    assert_fk_rejected(
        connection,
        invalid_operation=lambda db: core.insert_completion(
            db,
            first_prepared["batchId"],
            "fresh-origin",
            1,
            "none",
            first_receipt,
            None,
            second_prepared["effectDigest"],
        ),
        positive_control=lambda db: core.insert_completion(
            db,
            first_prepared["batchId"],
            "fresh-origin",
            1,
            "none",
            first_receipt,
            None,
            first_prepared["effectDigest"],
        ),
        expected=frozenset({completion_effect_fk}),
    )
    core.expect_integrity(
        lambda: connection.execute(
            "INSERT INTO fresh_origin_effects VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            (first_prepared["batchId"], 1, "fresh-origin", 1, "none",
             "handoff-s05-b", core.D22, "apply-s05-b",
             "reuse-final-custody", "primary", core.D00),
        ),
        "fresh-effect-batch-handoff-crossed",
    )
    connection.close()


@case("FO-S06")
def pure_fresh_apply_requires_completion_authorization() -> None:
    connection = apply_db()
    prepared = prepare_fresh_apply(
        connection, "s06", "reuse-final-custody", authorize=False
    )
    writes = connection.execute(
        "SELECT ordinal,relation_name,row_key,row_revision,post_digest "
        "FROM planned_writes WHERE apply_id=? ORDER BY ordinal",
        (prepared["applyId"],),
    ).fetchall()

    def insert_writes_and_marker(db: sqlite3.Connection) -> None:
        for row in writes:
            db.execute(
                "INSERT INTO semantic_writes VALUES(?,?,?,?,?,?)",
                (prepared["applyId"], *row),
            )
        insert_apply_marker(db, prepared)

    def authorize_and_apply(db: sqlite3.Connection) -> None:
        db.execute(
            "INSERT INTO batch_authorizations VALUES(?,?,?)",
            (
                prepared["batchId"],
                prepared["completionDigest"],
                prepared["authorizationDigest"],
            ),
        )
        insert_writes_and_marker(db)

    assert_fk_rejected(
        connection,
        invalid_operation=insert_writes_and_marker,
        positive_control=authorize_and_apply,
        expected=frozenset(
            {
                ForeignKeySpec(
                    "transition_applies",
                    ("batch_id", "completion_digest", "authorization_digest"),
                    "batch_authorizations",
                    ("batch_id", "completion_digest", "authorization_digest"),
                )
            }
        ),
    )
    require(
        fresh_post_counts(connection, prepared)["writes"] == prepared["writeCount"],
        "positive authorization control did not persist semantic rows",
    )
    require(
        fresh_post_counts(connection, prepared)["applies"] == 1,
        "positive authorization control did not persist the marker",
    )
    connection.close()


@case("FO-S07")
def authorized_pure_fresh_has_one_exact_post_state() -> None:
    connection = apply_db()
    prepared = prepare_fresh_apply(
        connection, "s07", "reuse-final-custody", authorize=True
    )
    apply_prepared(connection, prepared)
    apply_prepared(connection, prepared)
    counts = fresh_post_counts(connection, prepared)
    require(counts == {
        "writes": 2,
        "applies": 1,
        "source-custody": 0,
        "custody": 1,
        "generation-loss": 0,
        "commit": 1,
    }, f"pure fresh post-state: {counts}")
    require(connection.execute("SELECT row_revision FROM semantic_writes "
                               "WHERE apply_id=? AND relation_name='custody'",
                               (prepared["applyId"],)).fetchone() == (1,),
            "new custody is not revision one")
    table_counts = {
        table: connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
        for table in (
            "receipt_batches", "receipt_intents", "authority_receipts",
            "fresh_origin_effects", "batch_completions", "batch_authorizations",
            "transition_applies",
        )
    }
    require(all(value == 1 for value in table_counts.values()),
            f"pure fresh row cardinality: {table_counts}")
    require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
            "pure fresh FK check")
    connection.close()


@case("FO-S08")
def authorized_terminal_fresh_covers_optional_linked_loss() -> None:
    for linked in (False, True):
        connection = apply_db()
        prepared = prepare_fresh_apply(
            connection,
            f"s08-{'linked' if linked else 'plain'}",
            "terminalize-nonfinal-custody",
            linked_loss=linked,
            authorize=True,
        )
        apply_prepared(connection, prepared)
        counts = fresh_post_counts(connection, prepared)
        require(counts["source-custody"] == 1, "old custody not terminalized")
        require(counts["custody"] == 1 and counts["commit"] == 1,
                "terminal-fresh new custody/commit missing")
        require(counts["generation-loss"] == int(linked), "linked loss crossing")
        require(counts["writes"] == 3 + int(linked), "terminal-fresh write count")
        intent_count = connection.execute(
            "SELECT COUNT(*) FROM receipt_intents WHERE batch_id=?", (prepared["batchId"],)
        ).fetchone()[0]
        receipt_count = connection.execute(
            "SELECT COUNT(*) FROM authority_receipts WHERE batch_id=?", (prepared["batchId"],)
        ).fetchone()[0]
        effect_count = connection.execute(
            "SELECT COUNT(*) FROM nonfresh_effects WHERE batch_id=?", (prepared["batchId"],)
        ).fetchone()[0] + connection.execute(
            "SELECT COUNT(*) FROM fresh_origin_effects WHERE batch_id=?", (prepared["batchId"],)
        ).fetchone()[0]
        require((intent_count, receipt_count, effect_count) == (2, 2, 2 + int(linked)),
                "terminal-fresh batch cardinality")
        require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
                "terminal-fresh FK check")
        connection.close()


@case("FO-S09")
def adopted_review_arm_remains_nonfresh_and_valid() -> None:
    connection = apply_db()
    batch = core.insert_batch(
        connection, "s09", "custody-terminal", "review-adoption-decision", 2,
        True, None, False
    )
    first = core.insert_intent_and_receipt(
        connection, batch, 1, "custody-terminal", "custody-terminal", 2,
        "review-adoption-decision"
    )
    second = core.insert_intent_and_receipt(
        connection, batch, 2, "review-adoption-decision", "custody-terminal", 2,
        "review-adoption-decision"
    )
    core.insert_completion(
        connection, batch, "custody-terminal", 2, "review-adoption-decision",
        first, second, None
    )
    require(connection.execute(
        "SELECT COUNT(*) FROM fresh_origin_effects WHERE batch_id=?", (batch,)
    ).fetchone()[0] == 0, "adoption gained a fresh effect")
    require(connection.execute(
        "SELECT ordinal,kind FROM receipt_intents WHERE batch_id=? ORDER BY ordinal",
        (batch,),
    ).fetchall() == [(1, "custody-terminal"), (2, "review-adoption-decision")],
            "adoption ordinal regression")
    require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
            "adoption FK check")
    connection.close()


@case("FO-S10")
def every_apply_child_and_marker_fault_rolls_back_then_retries_once() -> None:
    modes = (
        ("reuse-final-custody", False),
        ("open-generation-loss", False),
        ("terminalize-nonfinal-custody", False),
        ("terminalize-nonfinal-custody", True),
    )
    for mode_index, (mode, linked) in enumerate(modes):
        probe = apply_db()
        probe_prepared = prepare_fresh_apply(
            probe, f"s10-probe-{mode_index}", mode, linked_loss=linked
        )
        boundary_count = probe_prepared["writeCount"] + 1
        probe.close()
        for boundary in range(1, boundary_count + 1):
            connection = apply_db()
            prepared = prepare_fresh_apply(
                connection, f"s10-{mode_index}-{boundary}", mode,
                linked_loss=linked, authorize=True
            )
            try:
                apply_prepared(connection, prepared, fault_before=boundary)
            except core.SimulatedCrash:
                pass
            else:
                raise core.OracleFailure(f"fault boundary committed: {mode}/{boundary}")
            require(fresh_post_counts(connection, prepared)["writes"] == 0,
                    f"fault left writes: {mode}/{boundary}")
            require(fresh_post_counts(connection, prepared)["applies"] == 0,
                    f"fault left marker: {mode}/{boundary}")
            apply_prepared(connection, prepared)
            apply_prepared(connection, prepared)
            require(fresh_post_counts(connection, prepared)["writes"]
                    == prepared["writeCount"], "retry write cardinality")
            require(fresh_post_counts(connection, prepared)["applies"] == 1,
                    "retry multiplied apply")
            connection.close()

    connection = apply_db()
    prepared = prepare_fresh_apply(
        connection, "s10-marker", "open-generation-loss", authorize=True
    )
    core.expect_integrity(
        lambda: insert_apply_marker(connection, prepared),
        "apply-children-incomplete",
    )
    connection.execute("BEGIN IMMEDIATE")
    first_rows = connection.execute(
        "SELECT ordinal,relation_name,row_key,row_revision,post_digest FROM planned_writes "
        "WHERE apply_id=? ORDER BY ordinal LIMIT ?",
        (prepared["applyId"], prepared["writeCount"] - 1),
    ).fetchall()
    for row in first_rows:
        connection.execute(
            "INSERT INTO semantic_writes VALUES(?,?,?,?,?,?)", (prepared["applyId"], *row)
        )
    core.expect_integrity(
        lambda: insert_apply_marker(connection, prepared),
        "apply-children-incomplete",
    )
    connection.rollback()
    first_planned = connection.execute(
        "SELECT ordinal,relation_name,row_key,row_revision,post_digest "
        "FROM planned_writes WHERE apply_id=? ORDER BY ordinal LIMIT 1",
        (prepared["applyId"],),
    ).fetchone()
    assert first_planned is not None
    apply_prepared(connection, prepared)
    connection.execute(
        "DELETE FROM semantic_writes WHERE apply_id=? AND ordinal=?",
        (prepared["applyId"], first_planned[0]),
    )
    assert_fk_rejected(
        connection,
        invalid_operation=lambda db: db.execute(
            "INSERT INTO semantic_writes VALUES(?,?,?,?,?,?)",
            (prepared["applyId"], 99, "commit", "extra", 1, core.D00),
        ),
        positive_control=lambda db: db.execute(
            "INSERT INTO semantic_writes VALUES(?,?,?,?,?,?)",
            (prepared["applyId"], *first_planned),
        ),
        expected=frozenset(
            {
                ForeignKeySpec(
                    "semantic_writes",
                    (
                        "apply_id",
                        "ordinal",
                        "relation_name",
                        "row_key",
                        "row_revision",
                        "post_digest",
                    ),
                    "planned_writes",
                    (
                        "apply_id",
                        "ordinal",
                        "relation_name",
                        "row_key",
                        "row_revision",
                        "post_digest",
                    ),
                )
            }
        ),
    )
    connection.close()


LOCAL_SCOPE_TABLES = (
    "scope_admission_outbox",
    "scope_admission_resolutions",
    "admitted_scopes",
    "scope_checkpoints",
    "scope_heads",
    "namespace_checkpoints",
    "namespace_members",
)


def local_scope_counts(path: Path) -> dict[str, int]:
    return core.table_counts(path, LOCAL_SCOPE_TABLES)


def pending_only(path: Path) -> bool:
    return local_scope_counts(path) == {
        "scope_admission_outbox": 1,
        "scope_admission_resolutions": 0,
        "admitted_scopes": 0,
        "scope_checkpoints": 0,
        "scope_heads": 0,
        "namespace_checkpoints": 0,
        "namespace_members": 0,
    }


def run_recovery_subprocess(local_path: Path, authority_path: Path) -> None:
    fixture_dir = Path(__file__).resolve().parent
    worker = (
        "from pathlib import Path; import sys; "
        "sys.path.insert(0,sys.argv[1]); "
        "import test_fresh_origin_after as c; "
        "c.recover_scope(Path(sys.argv[2]),Path(sys.argv[3]))"
    )
    result = subprocess.run(
        [sys.executable, "-c", worker, str(fixture_dir), str(local_path), str(authority_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    require(result.returncode == 0,
            f"scope recovery subprocess failed: {result.stderr.strip()}")


def crash_before_outbox_commit(local_path: Path) -> int:
    fixture_dir = Path(__file__).resolve().parent
    scope_json = core.jcs(core.SCOPE).decode("utf-8")
    digest = core.ld("admitted-scope", core.SCOPE)
    request_id = core.ld(
        "scope-admission-outbox", {"schemaVersion": 1, "scopeDigest": digest}
    )
    worker = (
        "import os,sqlite3,sys; "
        "p=sys.argv[1]; c=sqlite3.connect(p,isolation_level=None); "
        "c.execute('PRAGMA foreign_keys=ON'); c.execute('BEGIN IMMEDIATE'); "
        "c.execute('INSERT INTO scope_admission_outbox VALUES(?,?,?,?,?)',sys.argv[2:7]); "
        "os._exit(17)"
    )
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            worker,
            str(local_path),
            request_id,
            core.scope_key(core.SCOPE),
            scope_json,
            digest,
            "2026-07-14T00:00:01.000Z",
        ],
        cwd=fixture_dir,
        check=False,
    )
    return result.returncode


def resolution_values(
    request_id: str,
    digest: str,
    authority_result: dict[str, Any],
) -> tuple[str, str]:
    checkpoint = json.loads(authority_result["checkpointJson"])
    body = {
        "schemaVersion": 1,
        "admissionRequestId": request_id,
        "scopeDigest": digest,
        "initialScopeCheckpoint": checkpoint,
        "namespaceCheckpointDigest": authority_result["namespaceCheckpointDigest"],
        "namespaceMember": {
            "projectSessionId": core.SCOPE["projectSessionId"],
            "runId": core.SCOPE["runId"],
            "authorityId": core.SCOPE["authorityId"],
            "scopeCheckpointDigest": authority_result["checkpointDigest"],
            "receiptCountDec": "0",
            "headReceiptDigest": None,
        },
        "verifiedAt": "2026-07-14T00:00:02.000Z",
    }
    resolution_digest = core.ld("scope-admission-resolution", body)
    resolution_json = core.jcs(
        {**body, "resolutionDigest": resolution_digest}
    ).decode("utf-8")
    return resolution_json, resolution_digest


def finalize_scope_locked(
    local_path: Path,
    request_id: str,
    digest: str,
    authority_result: dict[str, Any],
) -> str:
    require(authority_result["receiptCount"] == 0, "initial checkpoint is nonzero")
    require(authority_result["headReceiptDigest"] is None, "initial checkpoint has head")
    resolution_json, resolution_digest = resolution_values(
        request_id, digest, authority_result
    )
    key = core.scope_key(core.SCOPE)
    connection = core.connect_file(local_path)
    connection.execute("PRAGMA busy_timeout=5000")
    connection.execute("BEGIN IMMEDIATE")
    existing = connection.execute(
        "SELECT resolution_digest,scope_digest,initial_checkpoint_digest,"
        "namespace_checkpoint_digest "
        "FROM scope_admission_resolutions WHERE admission_request_id=?",
        (request_id,),
    ).fetchone()
    if existing is not None:
        require(existing == (
            resolution_digest, digest, authority_result["checkpointDigest"],
            authority_result["namespaceCheckpointDigest"]
        ), "concurrent resolution crossed")
        connection.commit()
        connection.close()
        return "existing"
    inserts = (
        (
            "INSERT INTO admitted_scopes VALUES(?,?,?,?,?)",
            (key, request_id, digest, authority_result["checkpointDigest"], resolution_digest),
        ),
        (
            "INSERT INTO scope_checkpoints VALUES(?,?,?,?,?,?)",
            (key, authority_result["checkpointDigest"], 0, 0, None,
             authority_result["checkpointJson"]),
        ),
        (
            "INSERT INTO scope_heads VALUES(?,?,?)",
            (key, authority_result["checkpointDigest"], 1),
        ),
        (
            "INSERT INTO namespace_checkpoints VALUES(?,?,?,?,?,?,?)",
            (
                core.SCOPE["projectId"], core.SCOPE["authorityId"], 1,
                json.loads(authority_result["namespaceCheckpointJson"])[
                    "orderedScopeHeadSetDigest"
                ],
                authority_result["namespaceCheckpointJson"],
                authority_result["namespaceCheckpointDigest"],
                json.loads(authority_result["namespaceCheckpointJson"])["attestation"],
            ),
        ),
        (
            "INSERT INTO namespace_members VALUES(?,?,?,?,?,?)",
            (authority_result["namespaceCheckpointDigest"], 1, key,
             authority_result["checkpointDigest"], 0, None),
        ),
        (
            "INSERT INTO scope_admission_resolutions VALUES(?,?,?,?,?,?,?)",
            (
                request_id,
                key,
                digest,
                authority_result["checkpointDigest"],
                authority_result["namespaceCheckpointDigest"],
                resolution_json,
                resolution_digest,
            ),
        ),
    )
    for statement, parameters in inserts:
        connection.execute(statement, parameters)
    connection.commit()
    connection.close()
    return "inserted"


def hydration_status(
    local_path: Path,
    authority_path: Path,
    *,
    authorizer: Callable[[int, str | None, str | None, str | None, str | None], int]
    | None = None,
    authority_calls: list[str] | None = None,
) -> str:
    local = core.connect_file(local_path)
    if authorizer is not None:
        local.set_authorizer(authorizer)
    authority = core.connect_file(authority_path)
    if authority_calls is not None:
        authority_calls.extend(("readNamespaceCheckpoint", "readScopeCheckpoint"))
    try:
        outboxes = {
            row[1]: row
            for row in local.execute(
                "SELECT admission_request_id,scope_key,scope_json,scope_digest,created_at "
                "FROM scope_admission_outbox"
            ).fetchall()
        }
        resolutions = {
            row[1]: row
            for row in local.execute(
                "SELECT admission_request_id,scope_key,scope_digest,"
                "initial_checkpoint_digest,namespace_checkpoint_digest,"
                "resolution_json,resolution_digest FROM scope_admission_resolutions"
            ).fetchall()
        }
        admitted = {
            row[0]: row
            for row in local.execute(
                "SELECT scope_key,admission_request_id,scope_digest,"
                "initial_checkpoint_digest,resolution_digest FROM admitted_scopes"
            ).fetchall()
        }
        checkpoints = {
            row[0]: row
            for row in local.execute(
                "SELECT scope_key,checkpoint_digest,receipt_count,"
                "head_authority_sequence,head_receipt_digest,checkpoint_json "
                "FROM scope_checkpoints"
            ).fetchall()
        }
        heads = {
            row[0]: row
            for row in local.execute(
                "SELECT scope_key,checkpoint_digest,revision FROM scope_heads"
            ).fetchall()
        }
        local_namespace_checkpoints = {
            row[5]: row
            for row in local.execute(
                "SELECT project_id,authority_id,scope_count,"
                "ordered_scope_head_set_digest,checkpoint_json,checkpoint_digest,"
                "attestation FROM namespace_checkpoints"
            ).fetchall()
        }
        local_namespace_member_rows = local.execute(
            "SELECT checkpoint_digest,ordinal,scope_key,"
            "scope_checkpoint_digest,receipt_count,head_receipt_digest "
            "FROM namespace_members"
        ).fetchall()
        local_namespace_members = {
            (row[0], row[2]): row for row in local_namespace_member_rows
        }
        local_namespace_scope_keys = {row[2] for row in local_namespace_member_rows}
        local_namespace_member_sets: dict[str, list[tuple[Any, ...]]] = {}
        for row in local.execute(
            "SELECT checkpoint_digest,ordinal,scope_key,scope_checkpoint_digest,"
            "receipt_count,head_receipt_digest FROM namespace_members ORDER BY ordinal"
        ).fetchall():
            local_namespace_member_sets.setdefault(row[0], []).append(row)
        external_scopes = {
            row[0]: row
            for row in authority.execute(
                "SELECT scope_key,scope_json,scope_digest,checkpoint_json,"
                "checkpoint_digest FROM authority_scopes"
            ).fetchall()
        }
        namespace = {
            row[0]: row
            for row in authority.execute(
                "SELECT scope_key,checkpoint_digest,receipt_count,"
                "head_receipt_digest,namespace_checkpoint_digest,"
                "namespace_checkpoint_json "
                "FROM authority_namespace_members"
            ).fetchall()
        }
        external_namespace_checkpoints = {
            row[5]: row
            for row in authority.execute(
                "SELECT project_id,authority_id,scope_count,"
                "ordered_scope_head_set_digest,checkpoint_json,checkpoint_digest,"
                "attestation FROM authority_namespace_checkpoints"
            ).fetchall()
        }
        external_namespace_member_sets: dict[str, list[tuple[Any, ...]]] = {}
        external_namespace_member_bodies: dict[str, list[dict[str, Any]]] = {}
        for row in authority.execute(
            "SELECT checkpoint_digest,ordinal,scope_key,project_session_id,run_id,"
            "authority_id,scope_checkpoint_digest,receipt_count,head_receipt_digest "
            "FROM authority_namespace_snapshot_members ORDER BY ordinal"
        ).fetchall():
            external_namespace_member_sets.setdefault(row[0], []).append(
                (row[0], row[1], row[2], row[6], row[7], row[8])
            )
            external_namespace_member_bodies.setdefault(row[0], []).append({
                "projectSessionId": row[3],
                "runId": row[4],
                "authorityId": row[5],
                "scopeCheckpointDigest": row[6],
                "receiptCountDec": str(row[7]),
                "headReceiptDigest": row[8],
            })
        key_sets = (
            set(outboxes), set(resolutions), set(admitted), set(checkpoints),
            set(heads), local_namespace_scope_keys, set(external_scopes),
            set(namespace),
        )
        if not key_sets[0] or any(keys != key_sets[0] for keys in key_sets[1:]):
            return "SNAPSHOT_INVALID"
        for key in sorted(key_sets[0]):
            outbox = outboxes[key]
            resolution = resolutions[key]
            scope = admitted[key]
            checkpoint = checkpoints[key]
            head = heads[key]
            external = external_scopes[key]
            member = namespace[key]
            local_namespace_member = local_namespace_members.get((member[4], key))
            external_current_member = next(
                (row for row in external_namespace_member_sets.get(member[4], [])
                 if row[2] == key),
                None,
            )
            local_namespace_checkpoint = local_namespace_checkpoints.get(member[4])
            external_namespace_checkpoint = external_namespace_checkpoints.get(member[4])
            stored_scope = json.loads(outbox[2])
            expected_scope_digest = core.ld("admitted-scope", stored_scope)
            expected_request_id = core.ld(
                "scope-admission-outbox",
                {"schemaVersion": 1, "scopeDigest": expected_scope_digest},
            )
            checkpoint_json = json.loads(external[3])
            namespace_json = json.loads(member[5])
            namespace_body = {
                name: value for name, value in namespace_json.items()
                if name not in ("checkpointDigest", "attestation")
            }
            resolution_json = json.loads(resolution[5])
            conditions = (
                outbox[0] == expected_request_id,
                outbox[3] == expected_scope_digest,
                external[1] == outbox[2],
                external[2] == outbox[3],
                resolution[0] == outbox[0],
                resolution[2] == outbox[3],
                scope[1] == outbox[0],
                scope[2] == outbox[3],
                scope[3] == resolution[3] == external[4],
                scope[4] == resolution[6],
                checkpoint[1] == external[4],
                checkpoint[2:5] == (0, 0, None),
                checkpoint[5] == external[3],
                checkpoint_json.get("checkpointDigest") == external[4],
                head[1:] == (external[4], 1),
                member[1] == external[4],
                member[2:4] == (0, None),
                local_namespace_checkpoint is not None,
                external_namespace_checkpoint is not None,
                external_namespace_checkpoint is not None and
                    external_namespace_checkpoint == (
                        namespace_json["projectId"], namespace_json["authorityId"],
                        int(namespace_json["scopeCountDec"]),
                        namespace_json["orderedScopeHeadSetDigest"], member[5],
                        member[4], namespace_json["attestation"],
                    ),
                local_namespace_checkpoint == external_namespace_checkpoint,
                namespace_json.get("checkpointDigest") == member[4],
                namespace_json.get("attestation") == "opaque-authority-attestation",
                core.ld("namespace-checkpoint", namespace_body) == member[4],
                core.ld(
                    "namespace-scope-head-set",
                    external_namespace_member_bodies.get(member[4], []),
                ) == namespace_json["orderedScopeHeadSetDigest"],
                [row[1] for row in
                 external_namespace_member_sets.get(member[4], [])] ==
                    list(range(1, int(namespace_json["scopeCountDec"]) + 1)),
                local_namespace_member_sets.get(member[4]) ==
                    external_namespace_member_sets.get(member[4]),
                len(local_namespace_member_sets.get(member[4], ())) ==
                    int(namespace_json["scopeCountDec"]),
                local_namespace_member == external_current_member,
                resolution[4] == member[4],
                resolution_json.get("admissionRequestId") == outbox[0],
                resolution_json.get("scopeDigest") == outbox[3],
                resolution_json.get("resolutionDigest") == resolution[6],
                resolution_json.get("initialScopeCheckpoint", {}).get("checkpointDigest")
                    == external[4],
                resolution_json.get("namespaceMember", {}).get("scopeCheckpointDigest")
                    == external[4],
                resolution_json.get("namespaceMember", {}).get("receiptCountDec") == "0",
                resolution_json.get("namespaceMember", {}).get("headReceiptDigest") is None,
            )
            if not all(conditions):
                return "SNAPSHOT_INVALID"
        return "OK"
    except (json.JSONDecodeError, KeyError, TypeError, sqlite3.DatabaseError):
        return "SNAPSHOT_INVALID"
    finally:
        local.close()
        authority.close()


def startup_status(
    local_path: Path,
    authority_path: Path,
    *,
    authority_available: bool,
    hydration_calls: list[str],
) -> str:
    connection = core.connect_file(local_path)
    unresolved = connection.execute(
        "SELECT COUNT(*) FROM scope_admission_outbox AS o WHERE NOT EXISTS "
        "(SELECT 1 FROM scope_admission_resolutions AS r "
        "WHERE r.admission_request_id=o.admission_request_id)"
    ).fetchone()[0]
    connection.close()
    if unresolved:
        if not authority_available:
            return "RECOVERY_PENDING"
        core.recover_scope(local_path, authority_path)
    hydration_calls.append("hydrate")
    return hydration_status(local_path, authority_path)


def initialize_and_admit(directory: Path) -> tuple[Path, Path, str, str, dict[str, Any]]:
    local_path = directory / "local.sqlite3"
    authority_path = directory / "authority.sqlite3"
    core.initialize_scope_databases(local_path, authority_path)
    request_id, digest = core.stage_outbox(local_path, core.SCOPE)
    result = core.authority_admit(authority_path, core.SCOPE)
    return local_path, authority_path, request_id, digest, result


@case("SA-01")
def crash_before_outbox_commit_leaves_no_state_or_authority_call() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa01-matrix-") as directory:
        local_path = Path(directory) / "local.sqlite3"
        authority_path = Path(directory) / "authority.sqlite3"
        core.initialize_scope_databases(local_path, authority_path)
        require(crash_before_outbox_commit(local_path) == 17, "wrong forced-exit code")
        require(all(value == 0 for value in local_scope_counts(local_path).values()),
                "uncommitted outbox survived process exit")
        require(core.table_counts(authority_path, ("authority_scopes",))["authority_scopes"] == 0,
                "authority called before outbox commit")


@case("SA-02")
def committed_outbox_restarts_and_resolves_once() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa02-matrix-") as directory:
        local_path = Path(directory) / "local.sqlite3"
        authority_path = Path(directory) / "authority.sqlite3"
        core.initialize_scope_databases(local_path, authority_path)
        core.stage_outbox(local_path, core.SCOPE)
        require(pending_only(local_path), "outbox is not the only local row")
        run_recovery_subprocess(local_path, authority_path)
        require(all(value == 1 for value in local_scope_counts(local_path).values()),
                "restart did not finalize one scope")
        core.stage_outbox(local_path, core.SCOPE_TWO)
        run_recovery_subprocess(local_path, authority_path)
        require(local_scope_counts(local_path) == {
            "scope_admission_outbox": 2,
            "scope_admission_resolutions": 2,
            "admitted_scopes": 2,
            "scope_checkpoints": 2,
            "scope_heads": 2,
            "namespace_checkpoints": 2,
            "namespace_members": 3,
        }, "two-scope restart did not persist the complete pinned namespace")
        require(hydration_status(local_path, authority_path) == "OK",
                "two-scope restart did not hydrate from both pinned checkpoints")


@case("SA-03")
def aborted_authority_transaction_retries_to_one_external_scope() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa03-matrix-") as directory:
        root = Path(directory)
        local_path = root / "local.sqlite3"
        authority_path = root / "authority.sqlite3"
        scratch_path = root / "scratch.sqlite3"
        core.initialize_scope_databases(local_path, authority_path)
        scratch = sqlite3.connect(scratch_path)
        scratch.close()
        scratch_path.unlink()
        scratch_local = root / "scratch-local.sqlite3"
        core.initialize_scope_databases(scratch_local, scratch_path)
        result = core.authority_admit(scratch_path, core.SCOPE)
        core.stage_outbox(local_path, core.SCOPE)
        connection = core.connect_file(authority_path)
        connection.execute("BEGIN IMMEDIATE")
        key = core.scope_key(core.SCOPE)
        scope_json = core.jcs(core.SCOPE).decode("utf-8")
        digest = core.ld("admitted-scope", core.SCOPE)
        connection.execute(
            "INSERT INTO authority_scopes VALUES(?,?,?,?,?)",
            (key, scope_json, digest, result["checkpointJson"], result["checkpointDigest"]),
        )
        connection.execute(
            "INSERT INTO authority_namespace_members VALUES(?,?,?,?,?,?)",
            (key, result["checkpointDigest"], 0, None,
             result["namespaceCheckpointDigest"], result["namespaceCheckpointJson"]),
        )
        connection.rollback()
        connection.close()
        require(core.table_counts(authority_path, ("authority_scopes",))["authority_scopes"] == 0,
                "aborted authority transaction committed")
        require(pending_only(local_path), "authority abort changed local state")
        run_recovery_subprocess(local_path, authority_path)
        require(core.table_counts(authority_path, ("authority_scopes",))["authority_scopes"] == 1,
                "retry did not create one external scope")


@case("SA-04")
def lost_admit_response_returns_same_zero_checkpoint_and_one_resolution() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa04-matrix-") as directory:
        root = Path(directory)
        local_path = root / "local.sqlite3"
        authority_path = root / "authority.sqlite3"
        core.initialize_scope_databases(local_path, authority_path)
        core.stage_outbox(local_path, core.SCOPE)
        lost = core.authority_admit(authority_path, core.SCOPE)
        replay = core.authority_admit(authority_path, core.SCOPE)
        require(lost == replay, "exact admission replay changed zero checkpoint")
        run_recovery_subprocess(local_path, authority_path)
        require(all(value == 1 for value in local_scope_counts(local_path).values()),
                "lost response multiplied local rows")


@case("SA-05")
def crash_after_zero_checkpoint_before_namespace_proof_recovers_once() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa05-matrix-") as directory:
        root = Path(directory)
        local_path, authority_path, _, _, result = initialize_and_admit(root)
        require(result["receiptCount"] == 0 and result["headReceiptDigest"] is None,
                "authority did not return zero checkpoint")
        require(pending_only(local_path), "pre-proof crash wrote semantic state")
        run_recovery_subprocess(local_path, authority_path)
        require(all(value == 1 for value in local_scope_counts(local_path).values()),
                "pre-proof recovery not exact")


@case("SA-06")
def crash_after_namespace_proof_before_local_transaction_recovers_once() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa06-matrix-") as directory:
        root = Path(directory)
        local_path, authority_path, _, _, result = initialize_and_admit(root)
        connection = core.connect_file(authority_path)
        member = connection.execute(
            "SELECT checkpoint_digest,receipt_count,head_receipt_digest,"
            "namespace_checkpoint_digest FROM authority_namespace_members"
        ).fetchone()
        connection.close()
        require(member == (
            result["checkpointDigest"], 0, None, result["namespaceCheckpointDigest"]
        ), "namespace zero member proof crossed")
        require(pending_only(local_path), "namespace proof wrote local semantic state")
        run_recovery_subprocess(local_path, authority_path)
        require(all(value == 1 for value in local_scope_counts(local_path).values()),
                "post-proof recovery not exact")


@case("SA-07")
def each_local_insert_crash_rolls_back_then_restart_commits_all_six() -> None:
    for boundary in (1, 2, 3, 4, 5, 6):
        with tempfile.TemporaryDirectory(prefix=f"capa001-sa07-{boundary}-") as directory:
            root = Path(directory)
            local_path, authority_path, request_id, digest, result = initialize_and_admit(root)
            try:
                core.finalize_local(local_path, request_id, digest, result, boundary)
            except core.SimulatedCrash:
                pass
            else:
                raise core.OracleFailure(f"local boundary {boundary} committed")
            require(pending_only(local_path), f"partial local state at boundary {boundary}")
            run_recovery_subprocess(local_path, authority_path)
            require(all(value == 1 for value in local_scope_counts(local_path).values()),
                    f"boundary {boundary} did not recover exactly")


@case("SA-08")
def post_commit_ack_loss_replay_is_a_noop() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa08-matrix-") as directory:
        root = Path(directory)
        local_path, authority_path, request_id, digest, result = initialize_and_admit(root)
        core.finalize_local(local_path, request_id, digest, result)
        run_recovery_subprocess(local_path, authority_path)
        run_recovery_subprocess(local_path, authority_path)
        require(all(value == 1 for value in local_scope_counts(local_path).values()),
                "post-commit replay multiplied local rows")


@case("SA-09")
def two_workers_converge_on_one_authority_result_and_resolution() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa09-matrix-") as directory:
        root = Path(directory)
        local_path = root / "local.sqlite3"
        authority_path = root / "authority.sqlite3"
        core.initialize_scope_databases(local_path, authority_path)
        request_id, digest = core.stage_outbox(local_path, core.SCOPE)
        barrier = threading.Barrier(2)
        outcomes: list[str] = []
        errors: list[BaseException] = []

        def worker() -> None:
            try:
                result = core.authority_admit(authority_path, core.SCOPE)
                barrier.wait(timeout=5)
                outcomes.append(finalize_scope_locked(
                    local_path, request_id, digest, result
                ))
            except BaseException as error:  # captured for deterministic parent failure
                errors.append(error)

        threads = [threading.Thread(target=worker) for _ in range(2)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=10)
        require(not any(thread.is_alive() for thread in threads), "concurrent worker hung")
        require(errors == [], f"concurrent worker errors: {errors}")
        require(sorted(outcomes) == ["existing", "inserted"],
                f"concurrent finalization outcomes: {outcomes}")
        require(all(value == 1 for value in local_scope_counts(local_path).values()),
                "concurrent local uniqueness failed")
        require(core.table_counts(
            authority_path, ("authority_scopes", "authority_namespace_members")
        ) == {"authority_scopes": 1, "authority_namespace_members": 1},
                "authority exact replay multiplied scope")


@case("SA-10")
def changed_scope_byte_conflicts_without_local_admission() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa10-matrix-") as directory:
        root = Path(directory)
        local_path = root / "local.sqlite3"
        authority_path = root / "authority.sqlite3"
        core.initialize_scope_databases(local_path, authority_path)
        core.stage_outbox(local_path, core.SCOPE)
        core.authority_admit(authority_path, core.SCOPE)
        changed_scope = {**core.SCOPE, "admittedAt": "2026-07-14T00:00:03.000Z"}
        try:
            core.authority_admit(authority_path, changed_scope)
        except core.AdmissionConflict as error:
            require(str(error) == "LIFECYCLE_SCOPE_ADMISSION_CONFLICT",
                    "changed-byte conflict code drift")
        else:
            raise core.OracleFailure("authority accepted changed scope bytes")
        require(pending_only(local_path), "changed-byte conflict admitted locally")


@case("SA-11")
def unavailable_authority_blocks_startup_before_hydration() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa11-matrix-") as directory:
        root = Path(directory)
        local_path = root / "local.sqlite3"
        authority_path = root / "authority.sqlite3"
        core.initialize_scope_databases(local_path, authority_path)
        core.stage_outbox(local_path, core.SCOPE)
        hydration_calls: list[str] = []
        status = startup_status(
            local_path, authority_path, authority_available=False,
            hydration_calls=hydration_calls
        )
        require(status == "RECOVERY_PENDING", f"startup status: {status}")
        require(hydration_calls == [], "hydration ran while admission unresolved")
        require(pending_only(local_path), "unavailable startup wrote semantic state")


@case("SA-12")
def external_scope_without_local_outbox_is_snapshot_invalid() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa12-matrix-") as directory:
        root = Path(directory)
        local_path = root / "local.sqlite3"
        authority_path = root / "authority.sqlite3"
        core.initialize_scope_databases(local_path, authority_path)
        core.authority_admit(authority_path, core.SCOPE)
        require(hydration_status(local_path, authority_path) == "SNAPSHOT_INVALID",
                "hydration recreated externally orphaned scope")
        require(all(value == 0 for value in local_scope_counts(local_path).values()),
                "read-only hydration wrote orphaned scope")


@case("SA-13")
def nonzero_or_unknown_external_member_cannot_finalize_admission() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa13-matrix-") as directory:
        root = Path(directory)
        local_path, authority_path, _, _, _ = initialize_and_admit(root)
        connection = core.connect_file(authority_path)
        connection.execute("PRAGMA ignore_check_constraints=ON")
        connection.execute(
            "UPDATE authority_namespace_members SET receipt_count=1,head_receipt_digest=?",
            (core.D77,),
        )
        connection.close()
        try:
            core.recover_scope(local_path, authority_path)
        except core.OracleFailure as error:
            require("nonzero" in str(error), f"wrong nonzero recovery error: {error}")
        else:
            raise core.OracleFailure("nonzero initial member finalized")
        require(pending_only(local_path), "nonzero member created local semantic state")
        require(hydration_status(local_path, authority_path) == "SNAPSHOT_INVALID",
                "unknown external receipt passed hydration")


@case("SA-14")
def hydration_is_read_only_under_deny_write_authorizer() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa14-matrix-") as directory:
        root = Path(directory)
        local_path, authority_path, request_id, digest, result = initialize_and_admit(root)
        core.finalize_local(local_path, request_id, digest, result)
        denied_attempts: list[int] = []
        write_actions = {sqlite3.SQLITE_INSERT, sqlite3.SQLITE_UPDATE, sqlite3.SQLITE_DELETE}

        def deny_writes(
            action: int,
            _arg1: str | None,
            _arg2: str | None,
            _db: str | None,
            _trigger: str | None,
        ) -> int:
            if action in write_actions:
                denied_attempts.append(action)
                return sqlite3.SQLITE_DENY
            return sqlite3.SQLITE_OK

        authority_calls: list[str] = []
        status = hydration_status(
            local_path, authority_path, authorizer=deny_writes,
            authority_calls=authority_calls
        )
        require(status == "OK", f"read-only hydration status: {status}")
        require(denied_attempts == [], f"hydration attempted writes: {denied_attempts}")
        require("admitScope" not in authority_calls and "appendReceipt" not in authority_calls,
                f"hydration called a write port: {authority_calls}")
        require(authority_calls == ["readNamespaceCheckpoint", "readScopeCheckpoint"],
                f"hydration read-port trace: {authority_calls}")


@case("SA-X01")
def every_scope_admission_crossing_is_snapshot_invalid_and_read_only() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sax01-matrix-") as directory:
        root = Path(directory)
        baseline = root / "baseline"
        baseline.mkdir()
        local_path, authority_path, request_id, digest, result = initialize_and_admit(baseline)
        core.finalize_local(local_path, request_id, digest, result)
        second_request, second_digest = core.stage_outbox(local_path, core.SCOPE_TWO)
        core.finalize_local(
            local_path, second_request, second_digest,
            core.authority_admit(authority_path, core.SCOPE_TWO),
        )
        require(hydration_status(local_path, authority_path) == "OK",
                "two-scope crossing baseline is invalid")
        mutations: tuple[tuple[str, str, str], ...] = (
            ("missing-outbox", "local", "DROP TRIGGER scope_outbox_no_delete; DELETE FROM scope_admission_outbox;"),
            ("extra-outbox", "local", "INSERT INTO scope_admission_outbox VALUES('extra','extra-key','{}','sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','2026-07-14T00:00:09.000Z');"),
            ("missing-resolution", "local", "DROP TRIGGER scope_resolution_no_delete; DELETE FROM scope_admission_resolutions;"),
            ("crossed-resolution", "local", "DROP TRIGGER scope_resolution_no_update; UPDATE scope_admission_resolutions SET scope_digest='sha256:7777777777777777777777777777777777777777777777777777777777777777';"),
            ("crossed-admitted", "local", "UPDATE admitted_scopes SET resolution_digest='sha256:7777777777777777777777777777777777777777777777777777777777777777';"),
            ("crossed-checkpoint", "local", "UPDATE scope_checkpoints SET checkpoint_json='{}';"),
            ("crossed-head", "local", "UPDATE scope_heads SET revision=2;"),
            ("missing-local-namespace-checkpoint", "local", "DROP TRIGGER namespace_checkpoint_no_delete; DELETE FROM namespace_checkpoints;"),
            ("missing-local-namespace-member", "local", "DROP TRIGGER namespace_member_no_delete; DELETE FROM namespace_members;"),
            ("latest-namespace-missing-prior-member", "local", "DROP TRIGGER namespace_member_no_delete; DELETE FROM namespace_members WHERE checkpoint_digest=(SELECT checkpoint_digest FROM namespace_checkpoints WHERE scope_count=2) AND ordinal=1;"),
            ("crossed-local-namespace-ordered-digest", "local", "DROP TRIGGER namespace_checkpoint_no_update; UPDATE namespace_checkpoints SET ordered_scope_head_set_digest='sha256:7777777777777777777777777777777777777777777777777777777777777777';"),
            ("crossed-local-namespace-attestation", "local", "DROP TRIGGER namespace_checkpoint_no_update; UPDATE namespace_checkpoints SET attestation='crossed-attestation';"),
            ("latest-namespace-crossed-prior-ordinal", "local", "DROP TRIGGER namespace_member_no_update; UPDATE namespace_members SET ordinal=3 WHERE checkpoint_digest=(SELECT checkpoint_digest FROM namespace_checkpoints WHERE scope_count=2) AND ordinal=1;"),
            ("crossed-local-namespace-member", "local", "DROP TRIGGER namespace_member_no_update; UPDATE namespace_members SET scope_checkpoint_digest='sha256:7777777777777777777777777777777777777777777777777777777777777777';"),
            ("crossed-namespace", "authority", "UPDATE authority_namespace_members SET checkpoint_digest='sha256:7777777777777777777777777777777777777777777777777777777777777777';"),
            ("crossed-authority-scope", "authority", "UPDATE authority_scopes SET scope_digest='sha256:7777777777777777777777777777777777777777777777777777777777777777';"),
        )
        for index, (label, target, statement) in enumerate(mutations):
            case_dir = root / f"case-{index}"
            case_dir.mkdir()
            case_local = case_dir / "local.sqlite3"
            case_authority = case_dir / "authority.sqlite3"
            shutil.copy2(local_path, case_local)
            shutil.copy2(authority_path, case_authority)
            mutation_path = case_local if target == "local" else case_authority
            connection = sqlite3.connect(mutation_path, isolation_level=None)
            connection.execute("PRAGMA foreign_keys=OFF")
            connection.execute("PRAGMA ignore_check_constraints=ON")
            connection.executescript(statement)
            connection.close()
            before_bytes = case_local.read_bytes()
            require(hydration_status(case_local, case_authority) == "SNAPSHOT_INVALID",
                    f"scope crossing accepted: {label}")
            require(case_local.read_bytes() == before_bytes,
                    f"hydration changed local database: {label}")


def main() -> int:
    passed = 0
    for case_id, function in CASES:
        try:
            function()
        except Exception as error:
            print(f"{case_id}: FAIL: {type(error).__name__}: {error}")
            return 1
        print(f"{case_id}: PASS")
        passed += 1
    print(f"fresh-origin-matrix-after: {passed}/{len(CASES)}")
    return 0


def test_fresh_origin_matrix_after_oracle(capsys) -> None:
    assert main() == 0
    output = capsys.readouterr().out
    assert f"fresh-origin-matrix-after: {len(CASES)}/{len(CASES)}" in output


if __name__ == "__main__":
    raise SystemExit(main())
