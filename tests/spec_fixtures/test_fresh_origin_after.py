#!/usr/bin/env python3
"""Executable CAPA-001 Q1 after-repair oracle.

This fixture is intentionally self-contained and Python-stdlib only.  It models
the council-selected fresh-origin batch discriminator and the durable scope
admission outbox as executable SQLite constraints.  It is not production DDL;
it is a right-reason oracle for the normative lifecycle and hardening edits.

Stable output is one case ID per passing case followed by a fixed summary.
"""

from __future__ import annotations

import hashlib
import itertools
import json
from pathlib import Path
import re
import sqlite3
import tempfile
from typing import Any, Callable, Iterable


class OracleFailure(AssertionError):
    """A fixture invariant failed."""


class CodecError(ValueError):
    """A closed lifecycle codec rejected its input."""


class AdmissionConflict(RuntimeError):
    """The authority already owns the key with different exact scope bytes."""


class SimulatedCrash(RuntimeError):
    """A local transaction was deliberately abandoned before commit."""


Case = tuple[str, Callable[[], None]]
CASES: list[Case] = []


def case(case_id: str) -> Callable[[Callable[[], None]], Callable[[], None]]:
    def register(function: Callable[[], None]) -> Callable[[], None]:
        CASES.append((case_id, function))
        return function

    return register


def require(condition: bool, message: str) -> None:
    if not condition:
        raise OracleFailure(message)


def expect_integrity(
    operation: Callable[[], Any],
    exact_message: str,
) -> sqlite3.IntegrityError:
    try:
        operation()
    except sqlite3.IntegrityError as error:
        require(
            str(error) == exact_message,
            f"wrong SQLite reason: expected {exact_message!r}, got {str(error)!r}",
        )
        return error
    raise OracleFailure(f"expected {exact_message!r}, operation accepted")


# ---------------------------------------------------------------------------
# Restricted RFC 8785/JCS and exact lifecycle LD vectors.
# ---------------------------------------------------------------------------


DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
LD_PREFIX = b"agent-fabric.lifecycle.v1\x00"


def _validate_jcs_value(value: Any) -> None:
    if value is None or isinstance(value, (str, bool)):
        return
    if isinstance(value, int) and not isinstance(value, bool):
        if 0 <= value <= 9_007_199_254_740_991:
            return
        raise CodecError("integer outside lifecycle safe-integer range")
    if isinstance(value, list):
        for member in value:
            _validate_jcs_value(member)
        return
    if isinstance(value, dict):
        if not all(isinstance(key, str) for key in value):
            raise CodecError("JCS object keys must be strings")
        for member in value.values():
            _validate_jcs_value(member)
        return
    raise CodecError(f"unsupported lifecycle JCS value: {type(value).__name__}")


def jcs(value: Any) -> bytes:
    """RFC 8785 bytes for this fixture's integer/string/null-only vectors."""

    _validate_jcs_value(value)
    return json.dumps(
        value,
        ensure_ascii=False,
        allow_nan=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def ld(domain: str, value: Any) -> str:
    if not re.fullmatch(r"[a-z0-9-]+", domain):
        raise CodecError("LD domain is not exact lowercase ASCII")
    preimage = LD_PREFIX + domain.encode("ascii") + b"\x00" + jcs(value)
    return "sha256:" + hashlib.sha256(preimage).hexdigest()


def exact_keys(value: dict[str, Any], expected: Iterable[str], name: str) -> None:
    expected_set = set(expected)
    actual_set = set(value)
    missing = sorted(expected_set - actual_set)
    extra = sorted(actual_set - expected_set)
    if missing or extra:
        raise CodecError(f"{name} is not closed: missing={missing}, extra={extra}")


def assert_digest(value: str, label: str) -> None:
    if DIGEST_RE.fullmatch(value) is None:
        raise CodecError(f"{label} is not a sha256 digest")


def fixed_digest(byte: str) -> str:
    return "sha256:" + byte * 64


D00 = fixed_digest("0")
D11 = fixed_digest("1")
D22 = fixed_digest("2")
D33 = fixed_digest("3")
D44 = fixed_digest("4")
D55 = fixed_digest("5")
D66 = fixed_digest("6")
D77 = fixed_digest("7")


RECOVERY_SOURCE = {
    "schemaVersion": 1,
    "kind": "final-custody",
    "runId": "run-001",
    "agentId": "agent-001",
    "custodyId": "custody-source",
    "custodyRevision": 7,
}
NEW_CUSTODY_REF = {
    "schemaVersion": 1,
    "runId": "run-001",
    "agentId": "agent-001",
    "custodyId": "custody-new",
    "custodyRevision": 1,
}
FRESH_EFFECT_BODY = {
    "schemaVersion": 1,
    "effectKind": "fresh-origin",
    "role": "primary",
    "sourceMode": "reuse-final-custody",
    "recoverySource": RECOVERY_SOURCE,
    "sourceJournalDigest": D11,
    "freshHandoffDigest": D22,
    "admissionDigest": D33,
    "freshApplyPlanDigest": D44,
    "newCustodyRef": NEW_CUSTODY_REF,
    "newCustodySemanticDigest": D55,
    "newCustodySourceRefDigest": D66,
    "affectedGenerationLossBeforeRef": None,
    "affectedGenerationLossBeforeJournalDigest": None,
    "affectedGenerationLossAfterRef": None,
    "affectedGenerationLossAfterSemanticDigest": None,
}


FRESH_EFFECT_KEYS = (
    "schemaVersion",
    "effectKind",
    "role",
    "sourceMode",
    "recoverySource",
    "sourceJournalDigest",
    "freshHandoffDigest",
    "admissionDigest",
    "freshApplyPlanDigest",
    "newCustodyRef",
    "newCustodySemanticDigest",
    "newCustodySourceRefDigest",
    "affectedGenerationLossBeforeRef",
    "affectedGenerationLossBeforeJournalDigest",
    "affectedGenerationLossAfterRef",
    "affectedGenerationLossAfterSemanticDigest",
)


def validate_fresh_effect(effect: dict[str, Any]) -> None:
    exact_keys(effect, FRESH_EFFECT_KEYS, "lifecycleFreshOriginEffectV1")
    require(effect["schemaVersion"] == 1, "effect schema version")
    require(effect["effectKind"] == "fresh-origin", "effect kind")
    require(effect["role"] in ("primary", "secondary"), "effect role")
    require(
        effect["sourceMode"]
        in (
            "terminalize-nonfinal-custody",
            "reuse-final-custody",
            "open-generation-loss",
        ),
        "effect source mode",
    )
    for key in (
        "sourceJournalDigest",
        "freshHandoffDigest",
        "admissionDigest",
        "freshApplyPlanDigest",
        "newCustodySemanticDigest",
        "newCustodySourceRefDigest",
    ):
        assert_digest(effect[key], key)
    loss_values = [
        effect["affectedGenerationLossBeforeRef"],
        effect["affectedGenerationLossBeforeJournalDigest"],
        effect["affectedGenerationLossAfterRef"],
        effect["affectedGenerationLossAfterSemanticDigest"],
    ]
    require(all(value is None for value in loss_values) or all(value is not None for value in loss_values),
            "affected-loss quartet must be all-null or all-nonnull")
    if effect["sourceMode"] == "reuse-final-custody":
        require(all(value is None for value in loss_values), "reuse-final cannot carry loss")


validate_fresh_effect(FRESH_EFFECT_BODY)
FRESH_EFFECT_DIGEST = ld("lifecycle-effect", FRESH_EFFECT_BODY)
EFFECT_SET_DIGEST = ld("effect-set", [FRESH_EFFECT_DIGEST])


FRESH_REPLAY = {
    "schemaVersion": 1,
    "transactionId": "apply-fresh-001",
    "projectSessionId": "session-001",
    "runId": "run-001",
    "agentId": "agent-001",
    "transitionKind": "fresh-origin",
    "primaryOwnerBeforeRef": {
        "kind": "fresh-handoff",
        "handoffId": "handoff-001",
        "handoffDigest": D22,
    },
    "primaryOwnerAfterRef": {"kind": "custody", "custodyRef": NEW_CUSTODY_REF},
    "primaryOwnerBeforeJournalDigest": D11,
    "primaryOwnerAfterSemanticDigest": D55,
    "effectsSetDigest": EFFECT_SET_DIGEST,
    "admissionDigest": D33,
    "recoverySource": RECOVERY_SOURCE,
    "sourceMode": "reuse-final-custody",
    "freshHandoffDigest": D22,
    "freshApplyPlanDigest": D44,
    "affectedGenerationLossBeforeRef": None,
    "affectedGenerationLossBeforeJournalDigest": None,
    "affectedGenerationLossAfterRef": None,
    "affectedGenerationLossAfterSemanticDigest": None,
}
TRANSITION_REPLAY_DIGEST = ld("transition-replay", FRESH_REPLAY)


FRESH_SUBJECT = {
    "schemaVersion": 1,
    "kind": "fresh-origin",
    "projectSessionId": "session-001",
    "runId": "run-001",
    "agentId": "agent-001",
    "ownerRef": {
        "kind": "custody",
        "custodyRef": NEW_CUSTODY_REF,
        "sourceRefDigest": D66,
    },
    "sourceMode": "reuse-final-custody",
    "recoverySource": RECOVERY_SOURCE,
    "sourceJournalDigest": D11,
    "admissionDigest": D33,
    "freshHandoffDigest": D22,
    "freshApplyPlanDigest": D44,
    "affectedGenerationLossBeforeRef": None,
    "affectedGenerationLossBeforeJournalDigest": None,
    "affectedGenerationLossAfterRef": None,
    "affectedGenerationLossAfterSemanticDigest": None,
    "freshOriginEffectDigest": FRESH_EFFECT_DIGEST,
    "transitionReplayDigest": TRANSITION_REPLAY_DIGEST,
}
SUBJECT_DIGEST = ld("receipt-subject", FRESH_SUBJECT)
ORDERED_SUBJECT_SET_DIGEST = ld(
    "receipt-subject-set",
    [
        {
            "ordinalDec": "1",
            "kind": "fresh-origin",
            "ownerRefDigest": D77,
            "ownerRevisionDec": "1",
            "subjectDigest": SUBJECT_DIGEST,
        }
    ],
)


BATCH_BODY_KEYS = (
    "schemaVersion",
    "projectSessionId",
    "runId",
    "agentId",
    "plannedApplyId",
    "transitionKind",
    "primaryOwnerBeforeRef",
    "primaryOwnerAfterRef",
    "primaryOwnerBeforeJournalDigest",
    "primaryOwnerAfterSemanticDigest",
    "effectsSetDigest",
    "transitionReplayDigest",
    "orderedSubjectSetDigest",
    "receiptIntentCountDec",
    "secondaryIntentKind",
    "reviewReservationRef",
    "freshHandoffRef",
)
BATCH_BODY = {
    "schemaVersion": 1,
    "projectSessionId": "session-001",
    "runId": "run-001",
    "agentId": "agent-001",
    "plannedApplyId": "apply-fresh-001",
    "transitionKind": "fresh-origin",
    "primaryOwnerBeforeRef": {
        "kind": "fresh-handoff",
        "handoffId": "handoff-001",
        "handoffDigest": D22,
        "preparationId": "preparation-001",
        "preparationDigest": D00,
        "plannedApplyId": "apply-fresh-001",
        "sourceMode": "reuse-final-custody",
        "recoverySource": RECOVERY_SOURCE,
        "sourceJournalDigest": D11,
        "freshApplyPlanDigest": D44,
    },
    "primaryOwnerAfterRef": {
        "kind": "custody",
        "custodyRef": NEW_CUSTODY_REF,
        "sourceRefDigest": D66,
    },
    "primaryOwnerBeforeJournalDigest": D11,
    "primaryOwnerAfterSemanticDigest": D55,
    "effectsSetDigest": EFFECT_SET_DIGEST,
    "transitionReplayDigest": TRANSITION_REPLAY_DIGEST,
    "orderedSubjectSetDigest": ORDERED_SUBJECT_SET_DIGEST,
    "receiptIntentCountDec": "1",
    "secondaryIntentKind": "none",
    "reviewReservationRef": None,
    "freshHandoffRef": {
        "handoffId": "handoff-001",
        "handoffDigest": D22,
        "preparationId": "preparation-001",
        "preparationDigest": D00,
        "sourceMode": "reuse-final-custody",
        "freshApplyPlanDigest": D44,
    },
}


def batch_id(body: dict[str, Any]) -> str:
    exact_keys(body, BATCH_BODY_KEYS, "lifecycleIntegrityReceiptBatchIdBodyV1")
    require(body["schemaVersion"] == 1, "batch schema version")
    return ld("receipt-batch-id", body)


BATCH_ID = batch_id(BATCH_BODY)
INTENT_BODY = {
    "schemaVersion": 1,
    "batchId": BATCH_ID,
    "ordinalDec": "1",
    "kind": "fresh-origin",
    "subjectDigest": SUBJECT_DIGEST,
    "transitionReplayDigest": TRANSITION_REPLAY_DIGEST,
}
INTENT_DIGEST = ld("receipt-intent", INTENT_BODY)


# Checked-in literals: these must not be regenerated by the fixture itself.
EXPECTED_BATCH_JCS = (
    '{"agentId":"agent-001","effectsSetDigest":"sha256:fe968de00dce78668c94237e404f9b2226d5b39a212a9b42fb8a6404ada8f852",'
    '"freshHandoffRef":{"freshApplyPlanDigest":"sha256:4444444444444444444444444444444444444444444444444444444444444444",'
    '"handoffDigest":"sha256:2222222222222222222222222222222222222222222222222222222222222222",'
    '"handoffId":"handoff-001","preparationDigest":"sha256:0000000000000000000000000000000000000000000000000000000000000000",'
    '"preparationId":"preparation-001","sourceMode":"reuse-final-custody"},'
    '"orderedSubjectSetDigest":"sha256:d1a26f4b7fac5c7894841ea2148d10c2de809fc791d5392320ba03d1c56ef5a6",'
    '"plannedApplyId":"apply-fresh-001","primaryOwnerAfterRef":{"custodyRef":{"agentId":"agent-001",'
    '"custodyId":"custody-new","custodyRevision":1,"runId":"run-001","schemaVersion":1},'
    '"kind":"custody","sourceRefDigest":"sha256:6666666666666666666666666666666666666666666666666666666666666666"},'
    '"primaryOwnerAfterSemanticDigest":"sha256:5555555555555555555555555555555555555555555555555555555555555555",'
    '"primaryOwnerBeforeJournalDigest":"sha256:1111111111111111111111111111111111111111111111111111111111111111",'
    '"primaryOwnerBeforeRef":{"freshApplyPlanDigest":"sha256:4444444444444444444444444444444444444444444444444444444444444444",'
    '"handoffDigest":"sha256:2222222222222222222222222222222222222222222222222222222222222222",'
    '"handoffId":"handoff-001","kind":"fresh-handoff","plannedApplyId":"apply-fresh-001",'
    '"preparationDigest":"sha256:0000000000000000000000000000000000000000000000000000000000000000",'
    '"preparationId":"preparation-001","recoverySource":{"agentId":"agent-001","custodyId":"custody-source",'
    '"custodyRevision":7,"kind":"final-custody","runId":"run-001","schemaVersion":1},'
    '"sourceJournalDigest":"sha256:1111111111111111111111111111111111111111111111111111111111111111",'
    '"sourceMode":"reuse-final-custody"},"projectSessionId":"session-001","receiptIntentCountDec":"1",'
    '"reviewReservationRef":null,"runId":"run-001","schemaVersion":1,"secondaryIntentKind":"none",'
    '"transitionKind":"fresh-origin","transitionReplayDigest":"sha256:524ff180940c0269af0f1332f21c2aa7dee4932e3683d700c55d20885e481926"}'
)
EXPECTED_DIGESTS = {
    "effect": "sha256:5c73f117e89401db5cb8091d54dae93442389ce945d35beb23b522358b612a00",
    "effect-set": "sha256:fe968de00dce78668c94237e404f9b2226d5b39a212a9b42fb8a6404ada8f852",
    "replay": "sha256:524ff180940c0269af0f1332f21c2aa7dee4932e3683d700c55d20885e481926",
    "subject": "sha256:5545e78ab00c08cf70ceb077a816a9e0ecdee72e747a1638976cb997dfa2723c",
    "subject-set": "sha256:d1a26f4b7fac5c7894841ea2148d10c2de809fc791d5392320ba03d1c56ef5a6",
    "batch": "sha256:7d3139f3a0e5f411fda353e6aef6d0e6f56a1b4ec3b6825e76e73b9727e5376f",
    "intent": "sha256:d3b5bc9b71426a8ab901db963810d335f64ec45058ee6e0dc4600aeadbc2c31c",
}


# ---------------------------------------------------------------------------
# Minimal relational oracle for the seven legal batch/completion arms.
# ---------------------------------------------------------------------------


SCHEMA_SQL = r"""
PRAGMA foreign_keys=ON;

CREATE TABLE review_reservations(
  reservation_id TEXT PRIMARY KEY,
  reservation_digest TEXT NOT NULL,
  UNIQUE(reservation_id,reservation_digest)
);

CREATE TABLE fresh_handoffs(
  handoff_id TEXT PRIMARY KEY,
  handoff_digest TEXT NOT NULL,
  planned_apply_id TEXT NOT NULL UNIQUE,
  source_mode TEXT NOT NULL CHECK(source_mode IN
    ('terminalize-nonfinal-custody','reuse-final-custody','open-generation-loss')),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id,source_mode)
);

CREATE TABLE retirement_plans(
  retirement_id TEXT PRIMARY KEY,
  retirement_digest TEXT NOT NULL,
  planned_apply_id TEXT NOT NULL UNIQUE,
  UNIQUE(retirement_id,retirement_digest,planned_apply_id)
);

CREATE TABLE receipt_batches(
  batch_id TEXT PRIMARY KEY,
  planned_apply_id TEXT NOT NULL UNIQUE,
  transition_kind TEXT NOT NULL CHECK(transition_kind IN
    ('custody-terminal','generation-loss-terminal',
     'custody-recovery-retirement','fresh-origin')),
  receipt_intent_count INTEGER NOT NULL CHECK(receipt_intent_count IN (1,2)),
  secondary_intent_kind TEXT NOT NULL CHECK(secondary_intent_kind IN
    ('none','fresh-origin','review-adoption-decision')),
  review_reservation_id TEXT,
  review_reservation_digest TEXT,
  fresh_handoff_id TEXT,
  fresh_handoff_digest TEXT,
  fresh_handoff_source_mode TEXT CHECK(fresh_handoff_source_mode IN
    ('terminalize-nonfinal-custody','reuse-final-custody','open-generation-loss')),
  recovery_retirement_id TEXT,
  recovery_retirement_digest TEXT,
  transition_replay_digest TEXT NOT NULL,
  effects_set_digest TEXT NOT NULL,
  UNIQUE(batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  CHECK((review_reservation_id IS NULL)=(review_reservation_digest IS NULL)),
  CHECK((fresh_handoff_id IS NULL)=(fresh_handoff_digest IS NULL)),
  CHECK((fresh_handoff_id IS NULL)=(fresh_handoff_source_mode IS NULL)),
  CHECK((recovery_retirement_id IS NULL)=(recovery_retirement_digest IS NULL)),
  CHECK(
    (transition_kind='custody-terminal' AND secondary_intent_kind='none' AND
      receipt_intent_count=1 AND review_reservation_id IS NULL AND
      fresh_handoff_id IS NULL AND recovery_retirement_id IS NULL) OR
    (transition_kind='custody-terminal' AND
      secondary_intent_kind='review-adoption-decision' AND
      receipt_intent_count=2 AND review_reservation_id IS NOT NULL AND
      fresh_handoff_id IS NULL AND recovery_retirement_id IS NULL) OR
    (transition_kind='custody-terminal' AND
      secondary_intent_kind='fresh-origin' AND receipt_intent_count=2 AND
      review_reservation_id IS NULL AND fresh_handoff_id IS NOT NULL AND
      fresh_handoff_source_mode='terminalize-nonfinal-custody' AND
      recovery_retirement_id IS NULL) OR
    (transition_kind='generation-loss-terminal' AND
      secondary_intent_kind='none' AND receipt_intent_count=1 AND
      review_reservation_id IS NULL AND fresh_handoff_id IS NULL AND
      recovery_retirement_id IS NULL) OR
    (transition_kind='custody-recovery-retirement' AND
      secondary_intent_kind='none' AND receipt_intent_count=1 AND
      review_reservation_id IS NULL AND fresh_handoff_id IS NULL AND
      recovery_retirement_id IS NOT NULL) OR
    (transition_kind='fresh-origin' AND secondary_intent_kind='none' AND
      receipt_intent_count=1 AND review_reservation_id IS NULL AND
      fresh_handoff_id IS NOT NULL AND fresh_handoff_source_mode IN
        ('reuse-final-custody','open-generation-loss') AND
      recovery_retirement_id IS NULL)
  ),
  FOREIGN KEY(review_reservation_id,review_reservation_digest)
    REFERENCES review_reservations(reservation_id,reservation_digest),
  FOREIGN KEY(fresh_handoff_id,fresh_handoff_digest,planned_apply_id,
      fresh_handoff_source_mode)
    REFERENCES fresh_handoffs(
      handoff_id,handoff_digest,planned_apply_id,source_mode),
  FOREIGN KEY(recovery_retirement_id,recovery_retirement_digest,planned_apply_id)
    REFERENCES retirement_plans(retirement_id,retirement_digest,planned_apply_id)
);

CREATE TABLE fresh_origin_effects(
  batch_id TEXT NOT NULL,
  receipt_ordinal INTEGER NOT NULL,
  batch_transition_kind TEXT NOT NULL,
  batch_intent_count INTEGER NOT NULL,
  batch_secondary_intent_kind TEXT NOT NULL,
  handoff_id TEXT NOT NULL,
  handoff_digest TEXT NOT NULL,
  planned_apply_id TEXT NOT NULL,
  handoff_source_mode TEXT NOT NULL,
  effect_role TEXT NOT NULL CHECK(effect_role IN ('primary','secondary')),
  effect_digest TEXT NOT NULL,
  PRIMARY KEY(batch_id,receipt_ordinal),
  UNIQUE(batch_id,receipt_ordinal,effect_role,effect_digest),
  FOREIGN KEY(batch_id,batch_transition_kind,batch_intent_count,
      batch_secondary_intent_kind)
    REFERENCES receipt_batches(
      batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  FOREIGN KEY(handoff_id,handoff_digest,planned_apply_id,handoff_source_mode)
    REFERENCES fresh_handoffs(
      handoff_id,handoff_digest,planned_apply_id,source_mode),
  CHECK(
    (batch_transition_kind='fresh-origin' AND receipt_ordinal=1 AND
      effect_role='primary') OR
    (batch_transition_kind='custody-terminal' AND receipt_ordinal=2 AND
      effect_role='secondary')
  )
);

CREATE TABLE receipt_intents(
  batch_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN
    ('custody-terminal','generation-loss-terminal',
     'custody-recovery-retirement','fresh-origin','review-adoption-decision')),
  batch_transition_kind TEXT NOT NULL,
  batch_intent_count INTEGER NOT NULL,
  batch_secondary_intent_kind TEXT NOT NULL,
  subject_digest TEXT NOT NULL,
  intent_digest TEXT NOT NULL UNIQUE,
  PRIMARY KEY(batch_id,ordinal),
  UNIQUE(batch_id,ordinal,kind,intent_digest,subject_digest),
  FOREIGN KEY(batch_id,batch_transition_kind,batch_intent_count,
      batch_secondary_intent_kind)
    REFERENCES receipt_batches(
      batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  CHECK(
    (ordinal=1 AND kind=batch_transition_kind) OR
    (ordinal=2 AND batch_intent_count=2 AND
      batch_secondary_intent_kind<>'none' AND
      kind=batch_secondary_intent_kind)
  )
);

CREATE TABLE authority_receipts(
  batch_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  kind TEXT NOT NULL,
  intent_digest TEXT NOT NULL,
  subject_digest TEXT NOT NULL,
  receipt_digest TEXT NOT NULL UNIQUE,
  PRIMARY KEY(batch_id,ordinal),
  UNIQUE(batch_id,ordinal,kind,intent_digest,subject_digest,receipt_digest),
  FOREIGN KEY(batch_id,ordinal,kind,intent_digest,subject_digest)
    REFERENCES receipt_intents(batch_id,ordinal,kind,intent_digest,subject_digest)
);

CREATE TABLE batch_completions(
  batch_id TEXT PRIMARY KEY,
  transition_kind TEXT NOT NULL,
  receipt_intent_count INTEGER NOT NULL,
  secondary_intent_kind TEXT NOT NULL,
  ordinal_one INTEGER NOT NULL CHECK(ordinal_one=1),
  ordinal_one_kind TEXT NOT NULL,
  ordinal_one_intent_digest TEXT NOT NULL,
  ordinal_one_subject_digest TEXT NOT NULL,
  ordinal_one_receipt_digest TEXT NOT NULL,
  ordinal_two INTEGER,
  ordinal_two_kind TEXT,
  ordinal_two_intent_digest TEXT,
  ordinal_two_subject_digest TEXT,
  ordinal_two_receipt_digest TEXT,
  primary_effect_kind TEXT NOT NULL,
  primary_effect_digest TEXT NOT NULL,
  linked_loss_effect_digest TEXT,
  secondary_effect_kind TEXT,
  secondary_effect_digest TEXT,
  primary_fresh_ordinal INTEGER,
  primary_fresh_role TEXT,
  primary_fresh_digest TEXT,
  secondary_fresh_ordinal INTEGER,
  secondary_fresh_role TEXT,
  secondary_fresh_digest TEXT,
  completion_digest TEXT NOT NULL UNIQUE,
  FOREIGN KEY(batch_id,transition_kind,receipt_intent_count,secondary_intent_kind)
    REFERENCES receipt_batches(
      batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  FOREIGN KEY(batch_id,ordinal_one,ordinal_one_kind,
      ordinal_one_intent_digest,ordinal_one_subject_digest,
      ordinal_one_receipt_digest)
    REFERENCES authority_receipts(
      batch_id,ordinal,kind,intent_digest,subject_digest,receipt_digest),
  FOREIGN KEY(batch_id,ordinal_two,ordinal_two_kind,
      ordinal_two_intent_digest,ordinal_two_subject_digest,
      ordinal_two_receipt_digest)
    REFERENCES authority_receipts(
      batch_id,ordinal,kind,intent_digest,subject_digest,receipt_digest),
  FOREIGN KEY(batch_id,primary_fresh_ordinal,primary_fresh_role,
      primary_fresh_digest)
    REFERENCES fresh_origin_effects(
      batch_id,receipt_ordinal,effect_role,effect_digest),
  FOREIGN KEY(batch_id,secondary_fresh_ordinal,secondary_fresh_role,
      secondary_fresh_digest)
    REFERENCES fresh_origin_effects(
      batch_id,receipt_ordinal,effect_role,effect_digest),
  CHECK(
    (secondary_intent_kind='none' AND receipt_intent_count=1 AND
      ordinal_two IS NULL AND ordinal_two_kind IS NULL AND
      ordinal_two_intent_digest IS NULL AND ordinal_two_subject_digest IS NULL AND
      ordinal_two_receipt_digest IS NULL) OR
    (secondary_intent_kind<>'none' AND receipt_intent_count=2 AND
      ordinal_two=2 AND ordinal_two_kind=secondary_intent_kind AND
      ordinal_two_intent_digest IS NOT NULL AND ordinal_two_subject_digest IS NOT NULL AND
      ordinal_two_receipt_digest IS NOT NULL)
  ),
  CHECK(
    (transition_kind='custody-terminal' AND primary_effect_kind='custody' AND
      primary_fresh_ordinal IS NULL AND primary_fresh_role IS NULL AND
      primary_fresh_digest IS NULL AND
      ((secondary_intent_kind='fresh-origin' AND
          secondary_effect_kind='fresh-origin' AND
          secondary_effect_digest IS NOT NULL AND secondary_fresh_ordinal=2 AND
          secondary_fresh_role='secondary' AND
          secondary_fresh_digest=secondary_effect_digest) OR
       (secondary_intent_kind<>'fresh-origin' AND
          secondary_effect_kind IS NULL AND secondary_effect_digest IS NULL AND
          secondary_fresh_ordinal IS NULL AND secondary_fresh_role IS NULL AND
          secondary_fresh_digest IS NULL))) OR
    (transition_kind='generation-loss-terminal' AND
      primary_effect_kind='generation-loss' AND
      linked_loss_effect_digest IS NULL AND secondary_effect_kind IS NULL AND
      secondary_effect_digest IS NULL AND primary_fresh_ordinal IS NULL AND
      primary_fresh_role IS NULL AND primary_fresh_digest IS NULL AND
      secondary_fresh_ordinal IS NULL AND secondary_fresh_role IS NULL AND
      secondary_fresh_digest IS NULL) OR
    (transition_kind='custody-recovery-retirement' AND
      primary_effect_kind='recovery-retirement' AND
      linked_loss_effect_digest IS NULL AND secondary_effect_kind IS NULL AND
      secondary_effect_digest IS NULL AND primary_fresh_ordinal IS NULL AND
      primary_fresh_role IS NULL AND primary_fresh_digest IS NULL AND
      secondary_fresh_ordinal IS NULL AND secondary_fresh_role IS NULL AND
      secondary_fresh_digest IS NULL) OR
    (transition_kind='fresh-origin' AND primary_effect_kind='fresh-origin' AND
      linked_loss_effect_digest IS NULL AND secondary_effect_kind IS NULL AND
      secondary_effect_digest IS NULL AND primary_fresh_ordinal=1 AND
      primary_fresh_role='primary' AND
      primary_fresh_digest=primary_effect_digest AND
      secondary_fresh_ordinal IS NULL AND secondary_fresh_role IS NULL AND
      secondary_fresh_digest IS NULL)
  )
);
"""


def relational_db() -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:", isolation_level=None)
    connection.execute("PRAGMA foreign_keys=ON")
    connection.executescript(SCHEMA_SQL)
    return connection


BATCH_LEGAL_ARM_ERROR = """CHECK constraint failed: (transition_kind='custody-terminal' AND secondary_intent_kind='none' AND
      receipt_intent_count=1 AND review_reservation_id IS NULL AND
      fresh_handoff_id IS NULL AND recovery_retirement_id IS NULL) OR
    (transition_kind='custody-terminal' AND
      secondary_intent_kind='review-adoption-decision' AND
      receipt_intent_count=2 AND review_reservation_id IS NOT NULL AND
      fresh_handoff_id IS NULL AND recovery_retirement_id IS NULL) OR
    (transition_kind='custody-terminal' AND
      secondary_intent_kind='fresh-origin' AND receipt_intent_count=2 AND
      review_reservation_id IS NULL AND fresh_handoff_id IS NOT NULL AND
      fresh_handoff_source_mode='terminalize-nonfinal-custody' AND
      recovery_retirement_id IS NULL) OR
    (transition_kind='generation-loss-terminal' AND
      secondary_intent_kind='none' AND receipt_intent_count=1 AND
      review_reservation_id IS NULL AND fresh_handoff_id IS NULL AND
      recovery_retirement_id IS NULL) OR
    (transition_kind='custody-recovery-retirement' AND
      secondary_intent_kind='none' AND receipt_intent_count=1 AND
      review_reservation_id IS NULL AND fresh_handoff_id IS NULL AND
      recovery_retirement_id IS NOT NULL) OR
    (transition_kind='fresh-origin' AND secondary_intent_kind='none' AND
      receipt_intent_count=1 AND review_reservation_id IS NULL AND
      fresh_handoff_id IS NOT NULL AND fresh_handoff_source_mode IN
        ('reuse-final-custody','open-generation-loss') AND
      recovery_retirement_id IS NULL)"""

INTENT_ORDINAL_ERROR = """CHECK constraint failed: (ordinal=1 AND kind=batch_transition_kind) OR
    (ordinal=2 AND batch_intent_count=2 AND
      batch_secondary_intent_kind<>'none' AND
      kind=batch_secondary_intent_kind)"""

COMPLETION_ORDINAL_ERROR = """CHECK constraint failed: (secondary_intent_kind='none' AND receipt_intent_count=1 AND
      ordinal_two IS NULL AND ordinal_two_kind IS NULL AND
      ordinal_two_intent_digest IS NULL AND ordinal_two_subject_digest IS NULL AND
      ordinal_two_receipt_digest IS NULL) OR
    (secondary_intent_kind<>'none' AND receipt_intent_count=2 AND
      ordinal_two=2 AND ordinal_two_kind=secondary_intent_kind AND
      ordinal_two_intent_digest IS NOT NULL AND ordinal_two_subject_digest IS NOT NULL AND
      ordinal_two_receipt_digest IS NOT NULL)"""


def insert_parents(
    connection: sqlite3.Connection,
    suffix: str,
    planned_apply_id: str,
    review: bool,
    handoff_mode: str | None,
    retirement: bool,
) -> tuple[str | None, str | None, str | None]:
    review_id = None
    handoff_id = None
    retirement_id = None
    if review:
        review_id = f"review-{suffix}"
        connection.execute(
            "INSERT INTO review_reservations VALUES(?,?)", (review_id, D11)
        )
    if handoff_mode is not None:
        handoff_id = f"handoff-{suffix}"
        connection.execute(
            "INSERT INTO fresh_handoffs VALUES(?,?,?,?)",
            (handoff_id, D22, planned_apply_id, handoff_mode),
        )
    if retirement:
        retirement_id = f"retirement-{suffix}"
        connection.execute(
            "INSERT INTO retirement_plans VALUES(?,?,?)",
            (retirement_id, D33, planned_apply_id),
        )
    return review_id, handoff_id, retirement_id


def insert_batch(
    connection: sqlite3.Connection,
    suffix: str,
    transition_kind: str,
    secondary_kind: str,
    count: int,
    review: bool = False,
    handoff_mode: str | None = None,
    retirement: bool = False,
) -> str:
    planned_apply_id = f"apply-{suffix}"
    review_id, handoff_id, retirement_id = insert_parents(
        connection, suffix, planned_apply_id, review, handoff_mode, retirement
    )
    batch = f"batch-{suffix}"
    connection.execute(
        """INSERT INTO receipt_batches VALUES(
          ?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
        (
            batch,
            planned_apply_id,
            transition_kind,
            count,
            secondary_kind,
            review_id,
            D11 if review else None,
            handoff_id,
            D22 if handoff_mode is not None else None,
            handoff_mode,
            retirement_id,
            D33 if retirement else None,
            D44,
            D55,
        ),
    )
    return batch


def legal_arm(
    transition_kind: str,
    secondary_kind: str,
    count: int,
    review: bool,
    handoff_mode: str | None,
    retirement: bool,
) -> bool:
    return (
        transition_kind == "custody-terminal"
        and secondary_kind == "none"
        and count == 1
        and not review
        and handoff_mode is None
        and not retirement
    ) or (
        transition_kind == "custody-terminal"
        and secondary_kind == "review-adoption-decision"
        and count == 2
        and review
        and handoff_mode is None
        and not retirement
    ) or (
        transition_kind == "custody-terminal"
        and secondary_kind == "fresh-origin"
        and count == 2
        and not review
        and handoff_mode == "terminalize-nonfinal-custody"
        and not retirement
    ) or (
        transition_kind == "generation-loss-terminal"
        and secondary_kind == "none"
        and count == 1
        and not review
        and handoff_mode is None
        and not retirement
    ) or (
        transition_kind == "custody-recovery-retirement"
        and secondary_kind == "none"
        and count == 1
        and not review
        and handoff_mode is None
        and retirement
    ) or (
        transition_kind == "fresh-origin"
        and secondary_kind == "none"
        and count == 1
        and not review
        and handoff_mode in ("reuse-final-custody", "open-generation-loss")
        and not retirement
    )


def insert_intent_and_receipt(
    connection: sqlite3.Connection,
    batch: str,
    ordinal: int,
    kind: str,
    transition_kind: str,
    count: int,
    secondary_kind: str,
) -> tuple[str, str, str]:
    subject = fixed_digest("8" if ordinal == 1 else "9")
    intent = ld(
        "receipt-intent",
        {
            "schemaVersion": 1,
            "batchId": batch,
            "ordinalDec": str(ordinal),
            "kind": kind,
            "subjectDigest": subject,
            "transitionReplayDigest": D44,
        },
    )
    receipt = ld(
        "authenticated-receipt",
        {
            "schemaVersion": 1,
            "kind": kind,
            "authorityId": "authority-001",
            "authoritySequenceDec": str(ordinal),
            "previousReceiptDigest": None if ordinal == 1 else D00,
            "intentDigest": intent,
            "subjectDigest": subject,
        },
    )
    connection.execute(
        "INSERT INTO receipt_intents VALUES(?,?,?,?,?,?,?,?)",
        (
            batch,
            ordinal,
            kind,
            transition_kind,
            count,
            secondary_kind,
            subject,
            intent,
        ),
    )
    connection.execute(
        "INSERT INTO authority_receipts VALUES(?,?,?,?,?,?)",
        (batch, ordinal, kind, intent, subject, receipt),
    )
    return intent, subject, receipt


def insert_completion(
    connection: sqlite3.Connection,
    batch: str,
    transition_kind: str,
    count: int,
    secondary_kind: str,
    first: tuple[str, str, str],
    second: tuple[str, str, str] | None,
    fresh_effect_digest: str | None,
) -> None:
    first_intent, first_subject, first_receipt = first
    if second is None:
        second_values: tuple[Any, ...] = (None, None, None, None, None)
    else:
        second_intent, second_subject, second_receipt = second
        second_values = (
            2,
            secondary_kind,
            second_intent,
            second_subject,
            second_receipt,
        )

    if transition_kind == "custody-terminal":
        primary_kind = "custody"
        primary_digest = D11
        linked = None
        if secondary_kind == "fresh-origin":
            secondary_effect = ("fresh-origin", fresh_effect_digest)
            primary_fresh = (None, None, None)
            secondary_fresh = (2, "secondary", fresh_effect_digest)
        else:
            secondary_effect = (None, None)
            primary_fresh = (None, None, None)
            secondary_fresh = (None, None, None)
    elif transition_kind == "generation-loss-terminal":
        primary_kind, primary_digest, linked = "generation-loss", D11, None
        secondary_effect = (None, None)
        primary_fresh = secondary_fresh = (None, None, None)
    elif transition_kind == "custody-recovery-retirement":
        primary_kind, primary_digest, linked = "recovery-retirement", D11, None
        secondary_effect = (None, None)
        primary_fresh = secondary_fresh = (None, None, None)
    else:
        primary_kind, primary_digest, linked = "fresh-origin", fresh_effect_digest, None
        secondary_effect = (None, None)
        primary_fresh = (1, "primary", fresh_effect_digest)
        secondary_fresh = (None, None, None)

    values = (
        batch,
        transition_kind,
        count,
        secondary_kind,
        1,
        transition_kind,
        first_intent,
        first_subject,
        first_receipt,
        *second_values,
        primary_kind,
        primary_digest,
        linked,
        *secondary_effect,
        *primary_fresh,
        *secondary_fresh,
        ld("batch-completion", {"schemaVersion": 1, "batchId": batch}),
    )
    connection.execute(
        "INSERT INTO batch_completions VALUES(" + ",".join("?" for _ in values) + ")",
        values,
    )


# ---------------------------------------------------------------------------
# File-backed external authority and local immutable outbox crash oracle.
# ---------------------------------------------------------------------------


LOCAL_SCOPE_SQL = r"""
PRAGMA foreign_keys=ON;
CREATE TABLE scope_admission_outbox(
  admission_request_id TEXT PRIMARY KEY,
  scope_key TEXT NOT NULL UNIQUE,
  scope_json TEXT NOT NULL,
  scope_digest TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  UNIQUE(admission_request_id,scope_key,scope_digest)
);
CREATE TRIGGER scope_outbox_no_update
BEFORE UPDATE ON scope_admission_outbox
BEGIN SELECT RAISE(ABORT,'SCOPE_ADMISSION_OUTBOX_IMMUTABLE'); END;
CREATE TRIGGER scope_outbox_no_delete
BEFORE DELETE ON scope_admission_outbox
BEGIN SELECT RAISE(ABORT,'SCOPE_ADMISSION_OUTBOX_IMMUTABLE'); END;

CREATE TABLE scope_admission_resolutions(
  admission_request_id TEXT PRIMARY KEY,
  scope_key TEXT NOT NULL UNIQUE,
  scope_digest TEXT NOT NULL,
  initial_checkpoint_digest TEXT NOT NULL UNIQUE,
  namespace_checkpoint_digest TEXT NOT NULL,
  resolution_json TEXT NOT NULL,
  resolution_digest TEXT NOT NULL UNIQUE,
  UNIQUE(admission_request_id,resolution_digest,scope_key,scope_digest,
    initial_checkpoint_digest),
  FOREIGN KEY(admission_request_id,scope_key,scope_digest)
    REFERENCES scope_admission_outbox(admission_request_id,scope_key,scope_digest),
  FOREIGN KEY(scope_key,initial_checkpoint_digest)
    REFERENCES scope_checkpoints(scope_key,checkpoint_digest),
  FOREIGN KEY(namespace_checkpoint_digest)
    REFERENCES namespace_checkpoints(checkpoint_digest),
  FOREIGN KEY(namespace_checkpoint_digest,scope_key,initial_checkpoint_digest)
    REFERENCES namespace_members(
      checkpoint_digest,scope_key,scope_checkpoint_digest)
);
CREATE TRIGGER scope_resolution_no_update
BEFORE UPDATE ON scope_admission_resolutions
BEGIN SELECT RAISE(ABORT,'SCOPE_ADMISSION_RESOLUTION_IMMUTABLE'); END;
CREATE TRIGGER scope_resolution_no_delete
BEFORE DELETE ON scope_admission_resolutions
BEGIN SELECT RAISE(ABORT,'SCOPE_ADMISSION_RESOLUTION_IMMUTABLE'); END;

CREATE TABLE admitted_scopes(
  scope_key TEXT PRIMARY KEY,
  admission_request_id TEXT NOT NULL UNIQUE,
  scope_digest TEXT NOT NULL UNIQUE,
  initial_checkpoint_digest TEXT NOT NULL,
  resolution_digest TEXT NOT NULL,
  FOREIGN KEY(admission_request_id,resolution_digest,scope_key,scope_digest,
      initial_checkpoint_digest)
    REFERENCES scope_admission_resolutions(
      admission_request_id,resolution_digest,scope_key,scope_digest,
      initial_checkpoint_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE scope_checkpoints(
  scope_key TEXT NOT NULL,
  checkpoint_digest TEXT NOT NULL,
  receipt_count INTEGER NOT NULL CHECK(receipt_count=0),
  head_authority_sequence INTEGER NOT NULL CHECK(head_authority_sequence=0),
  head_receipt_digest TEXT CHECK(head_receipt_digest IS NULL),
  checkpoint_json TEXT NOT NULL,
  PRIMARY KEY(scope_key,checkpoint_digest),
  FOREIGN KEY(scope_key) REFERENCES admitted_scopes(scope_key)
);

CREATE TABLE scope_heads(
  scope_key TEXT PRIMARY KEY,
  checkpoint_digest TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision=1),
  FOREIGN KEY(scope_key,checkpoint_digest)
    REFERENCES scope_checkpoints(scope_key,checkpoint_digest)
);

CREATE TABLE namespace_checkpoints(
  project_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  scope_count INTEGER NOT NULL CHECK(scope_count>=1),
  ordered_scope_head_set_digest TEXT NOT NULL,
  checkpoint_json TEXT NOT NULL,
  checkpoint_digest TEXT PRIMARY KEY,
  attestation TEXT NOT NULL
);
CREATE TRIGGER namespace_checkpoint_no_update
BEFORE UPDATE ON namespace_checkpoints
BEGIN SELECT RAISE(ABORT,'NAMESPACE_CHECKPOINT_IMMUTABLE'); END;
CREATE TRIGGER namespace_checkpoint_no_delete
BEFORE DELETE ON namespace_checkpoints
BEGIN SELECT RAISE(ABORT,'NAMESPACE_CHECKPOINT_IMMUTABLE'); END;

CREATE TABLE namespace_members(
  checkpoint_digest TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal>=1),
  scope_key TEXT NOT NULL,
  scope_checkpoint_digest TEXT NOT NULL,
  receipt_count INTEGER NOT NULL CHECK(receipt_count=0),
  head_receipt_digest TEXT CHECK(head_receipt_digest IS NULL),
  PRIMARY KEY(checkpoint_digest,ordinal),
  UNIQUE(checkpoint_digest,scope_key,scope_checkpoint_digest),
  FOREIGN KEY(checkpoint_digest)
    REFERENCES namespace_checkpoints(checkpoint_digest),
  FOREIGN KEY(scope_key) REFERENCES admitted_scopes(scope_key),
  FOREIGN KEY(scope_key,scope_checkpoint_digest)
    REFERENCES scope_checkpoints(scope_key,checkpoint_digest)
);
CREATE TRIGGER namespace_member_no_update
BEFORE UPDATE ON namespace_members
BEGIN SELECT RAISE(ABORT,'NAMESPACE_MEMBER_IMMUTABLE'); END;
CREATE TRIGGER namespace_member_no_delete
BEFORE DELETE ON namespace_members
BEGIN SELECT RAISE(ABORT,'NAMESPACE_MEMBER_IMMUTABLE'); END;
"""


AUTHORITY_SCOPE_SQL = r"""
PRAGMA foreign_keys=ON;
CREATE TABLE authority_scopes(
  scope_key TEXT PRIMARY KEY,
  scope_json TEXT NOT NULL,
  scope_digest TEXT NOT NULL,
  checkpoint_json TEXT NOT NULL,
  checkpoint_digest TEXT NOT NULL UNIQUE
);
CREATE TABLE authority_namespace_members(
  scope_key TEXT PRIMARY KEY,
  checkpoint_digest TEXT NOT NULL,
  receipt_count INTEGER NOT NULL CHECK(receipt_count=0),
  head_receipt_digest TEXT CHECK(head_receipt_digest IS NULL),
  namespace_checkpoint_digest TEXT NOT NULL,
  namespace_checkpoint_json TEXT NOT NULL,
  FOREIGN KEY(scope_key) REFERENCES authority_scopes(scope_key),
  FOREIGN KEY(checkpoint_digest) REFERENCES authority_scopes(checkpoint_digest)
);
CREATE TABLE authority_namespace_checkpoints(
  project_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  scope_count INTEGER NOT NULL CHECK(scope_count>=1),
  ordered_scope_head_set_digest TEXT NOT NULL,
  checkpoint_json TEXT NOT NULL,
  checkpoint_digest TEXT PRIMARY KEY,
  attestation TEXT NOT NULL
);
CREATE TABLE authority_namespace_snapshot_members(
  checkpoint_digest TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal>=1),
  scope_key TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  authority_id TEXT NOT NULL,
  scope_checkpoint_digest TEXT NOT NULL,
  receipt_count INTEGER NOT NULL CHECK(receipt_count=0),
  head_receipt_digest TEXT CHECK(head_receipt_digest IS NULL),
  PRIMARY KEY(checkpoint_digest,ordinal),
  UNIQUE(checkpoint_digest,scope_key),
  FOREIGN KEY(checkpoint_digest)
    REFERENCES authority_namespace_checkpoints(checkpoint_digest),
  FOREIGN KEY(scope_key) REFERENCES authority_scopes(scope_key),
  FOREIGN KEY(scope_checkpoint_digest) REFERENCES authority_scopes(checkpoint_digest)
);
"""


SCOPE = {
    "schemaVersion": 1,
    "projectId": "project-001",
    "projectSessionId": "session-001",
    "runId": "run-001",
    "authorityId": "authority-001",
    "admissionDigest": D33,
    "admittedAt": "2026-07-14T00:00:00.000Z",
}

SCOPE_TWO = {
    **SCOPE,
    "projectSessionId": "session-002",
    "runId": "run-002",
    "admissionDigest": D44,
    "admittedAt": "2026-07-14T00:00:10.000Z",
}


def connect_file(path: Path) -> sqlite3.Connection:
    connection = sqlite3.connect(path, isolation_level=None)
    connection.execute("PRAGMA foreign_keys=ON")
    connection.execute("PRAGMA busy_timeout=1000")
    return connection


def initialize_scope_databases(local_path: Path, authority_path: Path) -> None:
    local = connect_file(local_path)
    local.executescript(LOCAL_SCOPE_SQL)
    local.close()
    authority = connect_file(authority_path)
    authority.executescript(AUTHORITY_SCOPE_SQL)
    authority.close()


def scope_key(scope: dict[str, Any]) -> str:
    return "|".join(
        (
            scope["projectId"],
            scope["projectSessionId"],
            scope["runId"],
            scope["authorityId"],
        )
    )


def stage_outbox(local_path: Path, scope: dict[str, Any]) -> tuple[str, str]:
    scope_bytes = jcs(scope).decode("utf-8")
    digest = ld("admitted-scope", scope)
    request_id = ld(
        "scope-admission-outbox", {"schemaVersion": 1, "scopeDigest": digest}
    )
    connection = connect_file(local_path)
    connection.execute("BEGIN IMMEDIATE")
    connection.execute(
        "INSERT INTO scope_admission_outbox VALUES(?,?,?,?,?)",
        (
            request_id,
            scope_key(scope),
            scope_bytes,
            digest,
            "2026-07-14T00:00:01.000Z",
        ),
    )
    connection.commit()
    connection.close()
    return request_id, digest


def _authority_result(
    connection: sqlite3.Connection, key: str, scope_row: tuple[Any, ...]
) -> dict[str, Any]:
    admission = connection.execute(
        "SELECT receipt_count,head_receipt_digest,namespace_checkpoint_digest,"
        "namespace_checkpoint_json FROM authority_namespace_members WHERE scope_key=?",
        (key,),
    ).fetchone()
    members = connection.execute(
        "SELECT project_session_id,run_id,authority_id,scope_checkpoint_digest,"
        "receipt_count,head_receipt_digest "
        "FROM authority_namespace_snapshot_members WHERE checkpoint_digest=? "
        "ORDER BY ordinal",
        (admission[2],),
    ).fetchall()
    return {
        "checkpointJson": scope_row[2],
        "checkpointDigest": scope_row[3],
        "receiptCount": admission[0],
        "headReceiptDigest": admission[1],
        "namespaceCheckpointDigest": admission[2],
        "namespaceCheckpointJson": admission[3],
        "namespaceMembers": [
            {
                "projectSessionId": row[0],
                "runId": row[1],
                "authorityId": row[2],
                "scopeCheckpointDigest": row[3],
                "receiptCountDec": str(row[4]),
                "headReceiptDigest": row[5],
            }
            for row in members
        ],
    }


def authority_admit(authority_path: Path, scope: dict[str, Any]) -> dict[str, Any]:
    key = scope_key(scope)
    scope_json = jcs(scope).decode("utf-8")
    digest = ld("admitted-scope", scope)
    connection = connect_file(authority_path)
    connection.execute("BEGIN IMMEDIATE")
    existing = connection.execute(
        "SELECT scope_json,scope_digest,checkpoint_json,checkpoint_digest "
        "FROM authority_scopes WHERE scope_key=?", (key,),
    ).fetchone()
    if existing is not None:
        if existing[0] != scope_json or existing[1] != digest:
            connection.rollback()
            connection.close()
            raise AdmissionConflict("LIFECYCLE_SCOPE_ADMISSION_CONFLICT")
        result = _authority_result(connection, key, existing)
        connection.commit()
        connection.close()
        return result

    checkpoint_body = {
        "schemaVersion": 1,
        "authorityId": scope["authorityId"],
        "projectSessionId": scope["projectSessionId"],
        "runId": scope["runId"],
        "receiptCountDec": "0",
        "headAuthoritySequenceDec": "0",
        "headReceiptDigest": None,
        "orderedRecordSetDigest": ld("scope-record-set", []),
    }
    checkpoint_digest = ld("scope-checkpoint", checkpoint_body)
    checkpoint = {
        **checkpoint_body,
        "checkpointDigest": checkpoint_digest,
        "attestation": "opaque-authority-attestation",
    }
    checkpoint_json = jcs(checkpoint).decode("utf-8")
    connection.execute(
        "INSERT INTO authority_scopes VALUES(?,?,?,?,?)",
        (key, scope_json, digest, checkpoint_json, checkpoint_digest),
    )
    scope_rows = connection.execute(
        "SELECT scope_key,scope_json,checkpoint_digest FROM authority_scopes"
    ).fetchall()
    scope_rows.sort(
        key=lambda row: (
            json.loads(row[1])["projectSessionId"], json.loads(row[1])["runId"]
        )
    )
    members = [
        {
            "projectSessionId": json.loads(row[1])["projectSessionId"],
            "runId": json.loads(row[1])["runId"],
            "authorityId": json.loads(row[1])["authorityId"],
            "scopeCheckpointDigest": row[2],
            "receiptCountDec": "0",
            "headReceiptDigest": None,
        }
        for row in scope_rows
    ]
    namespace_body = {
        "schemaVersion": 1,
        "authorityId": scope["authorityId"],
        "projectId": scope["projectId"],
        "scopeCountDec": str(len(members)),
        "orderedScopeHeadSetDigest": ld("namespace-scope-head-set", members),
    }
    namespace_digest = ld("namespace-checkpoint", namespace_body)
    namespace_checkpoint = {
        **namespace_body,
        "checkpointDigest": namespace_digest,
        "attestation": "opaque-authority-attestation",
    }
    namespace_checkpoint_json = jcs(namespace_checkpoint).decode("utf-8")
    connection.execute(
        "INSERT INTO authority_namespace_checkpoints VALUES(?,?,?,?,?,?,?)",
        (scope["projectId"], scope["authorityId"], len(members),
         namespace_body["orderedScopeHeadSetDigest"], namespace_checkpoint_json,
         namespace_digest, namespace_checkpoint["attestation"]),
    )
    for ordinal, (row, member) in enumerate(zip(scope_rows, members), start=1):
        connection.execute(
            "INSERT INTO authority_namespace_snapshot_members VALUES(?,?,?,?,?,?,?,?,?)",
            (namespace_digest, ordinal, row[0], member["projectSessionId"],
             member["runId"], member["authorityId"],
             member["scopeCheckpointDigest"], 0, None),
        )
    connection.execute(
        "INSERT INTO authority_namespace_members VALUES(?,?,?,?,?,?)",
        (key, checkpoint_digest, 0, None, namespace_digest,
         namespace_checkpoint_json),
    )
    result = _authority_result(
        connection, key, (scope_json, digest, checkpoint_json, checkpoint_digest)
    )
    connection.commit()
    connection.close()
    return result


def finalize_local(
    local_path: Path,
    request_id: str,
    digest: str,
    authority_result: dict[str, Any],
    crash_after: int | None = None,
) -> None:
    connection = connect_file(local_path)
    outbox = connection.execute(
        "SELECT scope_key,scope_json,scope_digest FROM scope_admission_outbox "
        "WHERE admission_request_id=?", (request_id,),
    ).fetchone()
    connection.close()
    require(outbox is not None, "missing local admission outbox")
    stored_scope = json.loads(outbox[1])
    key = scope_key(stored_scope)
    require(outbox == (key, jcs(stored_scope).decode("utf-8"), digest),
            "crossed local admission outbox")
    require(ld("admitted-scope", stored_scope) == digest, "invalid scope digest")
    require(authority_result["receiptCount"] == 0, "initial checkpoint is nonzero")
    require(authority_result["headReceiptDigest"] is None, "zero checkpoint has a head")
    checkpoint_json = authority_result["checkpointJson"]
    checkpoint = json.loads(checkpoint_json)
    checkpoint_body = {
        "schemaVersion": 1,
        "authorityId": stored_scope["authorityId"],
        "projectSessionId": stored_scope["projectSessionId"],
        "runId": stored_scope["runId"],
        "receiptCountDec": "0",
        "headAuthoritySequenceDec": "0",
        "headReceiptDigest": None,
        "orderedRecordSetDigest": ld("scope-record-set", []),
    }
    require(checkpoint == {
        **checkpoint_body,
        "checkpointDigest": authority_result["checkpointDigest"],
        "attestation": "opaque-authority-attestation",
    }, "crossed initial checkpoint")
    require(ld("scope-checkpoint", checkpoint_body) ==
            authority_result["checkpointDigest"], "invalid checkpoint digest")
    namespace_checkpoint_json = authority_result["namespaceCheckpointJson"]
    namespace_checkpoint = json.loads(namespace_checkpoint_json)
    namespace_members = authority_result.get("namespaceMembers")
    require(isinstance(namespace_members, list), "missing complete namespace members")
    require(1 <= len(namespace_members) <= 65_536, "namespace member bound exceeded")
    require(namespace_members == sorted(
        namespace_members,
        key=lambda member: (member["projectSessionId"], member["runId"]),
    ), "namespace members are not ordered")
    require(len({(member["projectSessionId"], member["runId"])
                 for member in namespace_members}) == len(namespace_members),
            "duplicate namespace member")
    namespace_member = {
        "projectSessionId": stored_scope["projectSessionId"],
        "runId": stored_scope["runId"],
        "authorityId": stored_scope["authorityId"],
        "scopeCheckpointDigest": authority_result["checkpointDigest"],
        "receiptCountDec": "0",
        "headReceiptDigest": None,
    }
    require(namespace_member in namespace_members, "exact admitted member is absent")
    require(all(member["authorityId"] == stored_scope["authorityId"] and
                member["receiptCountDec"] == "0" and
                member["headReceiptDigest"] is None
                for member in namespace_members), "crossed namespace member")
    namespace_body = {
        "schemaVersion": 1,
        "authorityId": stored_scope["authorityId"],
        "projectId": stored_scope["projectId"],
        "scopeCountDec": str(len(namespace_members)),
        "orderedScopeHeadSetDigest": ld(
            "namespace-scope-head-set", namespace_members
        ),
    }
    require(namespace_checkpoint == {
        **namespace_body,
        "checkpointDigest": authority_result["namespaceCheckpointDigest"],
        "attestation": "opaque-authority-attestation",
    }, "crossed namespace checkpoint")
    require(
        ld("namespace-checkpoint", namespace_body) ==
        authority_result["namespaceCheckpointDigest"],
        "invalid namespace checkpoint digest",
    )
    resolution_body = {
        "schemaVersion": 1,
        "admissionRequestId": request_id,
        "scopeDigest": digest,
        "initialScopeCheckpoint": checkpoint,
        "namespaceCheckpointDigest": authority_result["namespaceCheckpointDigest"],
        "namespaceMember": namespace_member,
        "verifiedAt": "2026-07-14T00:00:02.000Z",
    }
    resolution_digest = ld("scope-admission-resolution", resolution_body)
    resolution_json = jcs(
        {**resolution_body, "resolutionDigest": resolution_digest}
    ).decode("utf-8")
    member_rows = [
        (
            authority_result["namespaceCheckpointDigest"],
            ordinal,
            "|".join((stored_scope["projectId"], member["projectSessionId"],
                      member["runId"], member["authorityId"])),
            member["scopeCheckpointDigest"],
            int(member["receiptCountDec"]),
            member["headReceiptDigest"],
        )
        for ordinal, member in enumerate(namespace_members, start=1)
    ]

    connection = connect_file(local_path)
    existing = connection.execute(
        "SELECT resolution_digest,scope_digest,initial_checkpoint_digest,"
        "namespace_checkpoint_digest "
        "FROM scope_admission_resolutions WHERE admission_request_id=?",
        (request_id,),
    ).fetchone()
    if existing is not None:
        require(
            existing == (
                resolution_digest,
                digest,
                authority_result["checkpointDigest"],
                authority_result["namespaceCheckpointDigest"],
            ),
            "crossed existing local resolution",
        )
        stored_checkpoint = connection.execute(
            "SELECT project_id,authority_id,scope_count,"
            "ordered_scope_head_set_digest,checkpoint_json,checkpoint_digest,"
            "attestation FROM namespace_checkpoints WHERE checkpoint_digest=?",
            (authority_result["namespaceCheckpointDigest"],),
        ).fetchone()
        require(stored_checkpoint == (
            stored_scope["projectId"], stored_scope["authorityId"],
            len(namespace_members), namespace_body["orderedScopeHeadSetDigest"],
            namespace_checkpoint_json, authority_result["namespaceCheckpointDigest"],
            namespace_checkpoint["attestation"],
        ), "crossed stored namespace checkpoint")
        stored_members = connection.execute(
            "SELECT checkpoint_digest,ordinal,scope_key,scope_checkpoint_digest,"
            "receipt_count,head_receipt_digest FROM namespace_members "
            "WHERE checkpoint_digest=? ORDER BY ordinal",
            (authority_result["namespaceCheckpointDigest"],),
        ).fetchall()
        require(stored_members == member_rows, "crossed stored namespace members")
        connection.close()
        return

    connection.execute("BEGIN IMMEDIATE")
    inserts: list[tuple[str, tuple[Any, ...]]] = [
        (
            "INSERT INTO admitted_scopes VALUES(?,?,?,?,?)",
            (key, request_id, digest, authority_result["checkpointDigest"], resolution_digest),
        ),
        (
            "INSERT INTO scope_checkpoints VALUES(?,?,?,?,?,?)",
            (key, authority_result["checkpointDigest"], 0, 0, None, checkpoint_json),
        ),
        (
            "INSERT INTO scope_heads VALUES(?,?,?)",
            (key, authority_result["checkpointDigest"], 1),
        ),
        (
            "INSERT INTO namespace_checkpoints VALUES(?,?,?,?,?,?,?)",
            (
                stored_scope["projectId"],
                stored_scope["authorityId"],
                len(namespace_members),
                namespace_checkpoint["orderedScopeHeadSetDigest"],
                namespace_checkpoint_json,
                authority_result["namespaceCheckpointDigest"],
                namespace_checkpoint["attestation"],
            ),
        ),
    ]
    inserts.extend(
        ("INSERT INTO namespace_members VALUES(?,?,?,?,?,?)", row)
        for row in member_rows
    )
    inserts.append((
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
        ))
    for index, (statement, parameters) in enumerate(inserts, start=1):
        connection.execute(statement, parameters)
        if crash_after == index:
            connection.close()  # SQLite rolls the open transaction back.
            raise SimulatedCrash(f"after-local-insert-{index}")
    connection.commit()
    require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
            "local scope FK check is not empty")
    connection.close()


def recover_scope(local_path: Path, authority_path: Path) -> None:
    connection = connect_file(local_path)
    pending = connection.execute(
        """SELECT admission_request_id,scope_json,scope_digest
           FROM scope_admission_outbox AS o
           WHERE NOT EXISTS(
             SELECT 1 FROM scope_admission_resolutions AS r
             WHERE r.admission_request_id=o.admission_request_id)"""
    ).fetchall()
    connection.close()
    for request_id, scope_json, digest in pending:
        stored_scope = json.loads(scope_json)
        result = authority_admit(authority_path, stored_scope)
        finalize_local(local_path, request_id, digest, result)


def table_counts(path: Path, tables: Iterable[str]) -> dict[str, int]:
    connection = connect_file(path)
    result = {
        table: connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
        for table in tables
    }
    connection.close()
    return result


# ---------------------------------------------------------------------------
# Cases.
# ---------------------------------------------------------------------------


@case("FO-C01")
def codec_and_digest_goldens_are_exact() -> None:
    require(jcs(BATCH_BODY).decode("utf-8") == EXPECTED_BATCH_JCS, "batch JCS drift")
    actual = {
        "effect": FRESH_EFFECT_DIGEST,
        "effect-set": EFFECT_SET_DIGEST,
        "replay": TRANSITION_REPLAY_DIGEST,
        "subject": SUBJECT_DIGEST,
        "subject-set": ORDERED_SUBJECT_SET_DIGEST,
        "batch": BATCH_ID,
        "intent": INTENT_DIGEST,
    }
    require(actual == EXPECTED_DIGESTS, f"digest golden drift: {actual}")
    permuted = dict(reversed(list(BATCH_BODY.items())))
    require(batch_id(permuted) == BATCH_ID, "JCS key permutation changed digest")

    for key in BATCH_BODY_KEYS:
        mutant = dict(BATCH_BODY)
        value = mutant[key]
        if value is None:
            mutant[key] = "unexpected"
        elif isinstance(value, int):
            mutant[key] = value + 1
        elif isinstance(value, str):
            mutant[key] = value + "-mutant"
        else:
            mutant[key] = {"mutant": True}
        require(
            ld("receipt-batch-id", mutant) != BATCH_ID,
            f"batch member not authenticated: {key}",
        )

    missing = dict(BATCH_BODY)
    missing.pop("secondaryIntentKind")
    try:
        batch_id(missing)
    except CodecError:
        pass
    else:
        raise OracleFailure("missing batch member parsed")
    extra = {**BATCH_BODY, "batchId": BATCH_ID}
    try:
        batch_id(extra)
    except CodecError:
        pass
    else:
        raise OracleFailure("excluded batchId parsed into its own preimage")


@case("FO-S01")
def exactly_seven_batch_arms_accept() -> None:
    connection = relational_db()
    arms = (
        ("ordinary", "custody-terminal", "none", 1, False, None, False),
        ("adopted", "custody-terminal", "review-adoption-decision", 2, True, None, False),
        ("terminal-fresh", "custody-terminal", "fresh-origin", 2, False,
         "terminalize-nonfinal-custody", False),
        ("loss", "generation-loss-terminal", "none", 1, False, None, False),
        ("retirement", "custody-recovery-retirement", "none", 1, False, None, True),
        ("fresh-reuse", "fresh-origin", "none", 1, False,
         "reuse-final-custody", False),
        ("fresh-open", "fresh-origin", "none", 1, False,
         "open-generation-loss", False),
    )
    for suffix, transition, secondary, count, review, handoff, retirement in arms:
        insert_batch(
            connection, suffix, transition, secondary, count, review, handoff, retirement
        )
    require(connection.execute("SELECT COUNT(*) FROM receipt_batches").fetchone()[0] == 7,
            "not all seven legal rows accepted")
    require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
            "truth-table FK check is not empty")
    connection.close()


@case("FO-S02")
def every_non_table_batch_combination_rejects_by_check() -> None:
    connection = relational_db()
    transitions = (
        "custody-terminal",
        "generation-loss-terminal",
        "custody-recovery-retirement",
        "fresh-origin",
    )
    secondaries = ("none", "fresh-origin", "review-adoption-decision")
    modes = (
        None,
        "terminalize-nonfinal-custody",
        "reuse-final-custody",
        "open-generation-loss",
    )
    accepted = 0
    rejected = 0
    for index, values in enumerate(
        itertools.product(transitions, secondaries, (1, 2), (False, True), modes, (False, True))
    ):
        transition, secondary, count, review, mode, retirement = values
        expected = legal_arm(transition, secondary, count, review, mode, retirement)
        operation = lambda index=index, values=values: insert_batch(
            connection,
            f"cart-{index}",
            values[0],
            values[1],
            values[2],
            values[3],
            values[4],
            values[5],
        )
        if expected:
            operation()
            accepted += 1
        else:
            expect_integrity(operation, BATCH_LEGAL_ARM_ERROR)
            rejected += 1
    require(accepted == 7, f"cartesian oracle admitted {accepted}, expected 7")
    require(rejected == 377, f"cartesian oracle rejected {rejected}, expected 377")
    require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
            "cartesian FK check is not empty")
    connection.close()


@case("FO-S03")
def pure_fresh_is_count_one_primary_and_terminal_fresh_has_ordinal_two() -> None:
    connection = relational_db()

    pure = insert_batch(
        connection, "pure", "fresh-origin", "none", 1, False,
        "reuse-final-custody", False
    )
    connection.execute(
        "INSERT INTO fresh_origin_effects VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        (pure, 1, "fresh-origin", 1, "none", "handoff-pure", D22,
         "apply-pure", "reuse-final-custody", "primary", D66),
    )
    first = insert_intent_and_receipt(
        connection, pure, 1, "fresh-origin", "fresh-origin", 1, "none"
    )
    insert_completion(connection, pure, "fresh-origin", 1, "none", first, None, D66)

    terminal = insert_batch(
        connection, "terminal", "custody-terminal", "fresh-origin", 2,
        False, "terminalize-nonfinal-custody", False
    )
    connection.execute(
        "INSERT INTO fresh_origin_effects VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        (terminal, 2, "custody-terminal", 2, "fresh-origin", "handoff-terminal",
         D22, "apply-terminal", "terminalize-nonfinal-custody", "secondary", D77),
    )
    terminal_first = insert_intent_and_receipt(
        connection, terminal, 1, "custody-terminal", "custody-terminal", 2,
        "fresh-origin"
    )

    def wrong_ordinal_two() -> None:
        insert_intent_and_receipt(
            connection, terminal, 2, "review-adoption-decision",
            "custody-terminal", 2, "fresh-origin"
        )

    expect_integrity(wrong_ordinal_two, INTENT_ORDINAL_ERROR)

    def incomplete_completion() -> None:
        insert_completion(
            connection, terminal, "custody-terminal", 2, "fresh-origin",
            terminal_first, None, D77
        )

    expect_integrity(incomplete_completion, COMPLETION_ORDINAL_ERROR)
    terminal_second = insert_intent_and_receipt(
        connection, terminal, 2, "fresh-origin", "custody-terminal", 2,
        "fresh-origin"
    )
    insert_completion(
        connection, terminal, "custody-terminal", 2, "fresh-origin",
        terminal_first, terminal_second, D77
    )
    rows = connection.execute(
        "SELECT batch_id,ordinal,kind FROM receipt_intents ORDER BY batch_id,ordinal"
    ).fetchall()
    require(
        rows == [
            ("batch-pure", 1, "fresh-origin"),
            ("batch-terminal", 1, "custody-terminal"),
            ("batch-terminal", 2, "fresh-origin"),
        ],
        f"wrong intent shape: {rows}",
    )
    require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
            "fresh completion FK check is not empty")
    connection.close()


@case("FO-S04")
def preparation_dependencies_force_handoff_then_batch_then_effect_then_intents() -> None:
    connection = relational_db()

    def batch_without_handoff() -> None:
        connection.execute(
            """INSERT INTO receipt_batches VALUES(
              'batch-order','apply-order','fresh-origin',1,'none',
              NULL,NULL,'handoff-order',?,'reuse-final-custody',NULL,NULL,?,?)""",
            (D22, D44, D55),
        )

    expect_integrity(batch_without_handoff, "FOREIGN KEY constraint failed")
    connection.execute(
        "INSERT INTO fresh_handoffs VALUES(?,?,?,?)",
        ("handoff-order", D22, "apply-order", "reuse-final-custody"),
    )

    def effect_before_batch() -> None:
        connection.execute(
            "INSERT INTO fresh_origin_effects VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            ("batch-order", 1, "fresh-origin", 1, "none", "handoff-order", D22,
             "apply-order", "reuse-final-custody", "primary", D66),
        )

    expect_integrity(effect_before_batch, "FOREIGN KEY constraint failed")
    batch_without_handoff()
    connection.execute(
        "INSERT INTO fresh_origin_effects VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        ("batch-order", 1, "fresh-origin", 1, "none", "handoff-order", D22,
         "apply-order", "reuse-final-custody", "primary", D66),
    )
    insert_intent_and_receipt(
        connection, "batch-order", 1, "fresh-origin", "fresh-origin", 1, "none"
    )
    require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
            "prepare-order FK check is not empty")
    connection.close()


@case("SA-01")
def outbox_is_the_only_pre_authority_write_and_is_immutable() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa01-") as directory:
        local_path = Path(directory) / "local.sqlite3"
        authority_path = Path(directory) / "authority.sqlite3"
        initialize_scope_databases(local_path, authority_path)
        stage_outbox(local_path, SCOPE)
        local_counts = table_counts(
            local_path,
            (
                "scope_admission_outbox",
                "scope_admission_resolutions",
                "admitted_scopes",
                "scope_checkpoints",
                "scope_heads",
                "namespace_checkpoints",
                "namespace_members",
            ),
        )
        require(local_counts == {
            "scope_admission_outbox": 1,
            "scope_admission_resolutions": 0,
            "admitted_scopes": 0,
            "scope_checkpoints": 0,
            "scope_heads": 0,
            "namespace_checkpoints": 0,
            "namespace_members": 0,
        }, f"pre-authority semantic write: {local_counts}")
        require(table_counts(authority_path, ("authority_scopes",))["authority_scopes"] == 0,
                "authority called before worker")

        connection = connect_file(local_path)
        expect_integrity(
            lambda: connection.execute(
                "UPDATE scope_admission_outbox SET created_at=created_at"
            ),
            "SCOPE_ADMISSION_OUTBOX_IMMUTABLE",
        )
        expect_integrity(
            lambda: connection.execute("DELETE FROM scope_admission_outbox"),
            "SCOPE_ADMISSION_OUTBOX_IMMUTABLE",
        )
        connection.close()


@case("SA-02")
def two_scope_lost_authority_responses_replay_exactly_once() -> None:
    with tempfile.TemporaryDirectory(prefix="capa001-sa02-") as directory:
        local_path = Path(directory) / "local.sqlite3"
        authority_path = Path(directory) / "authority.sqlite3"
        initialize_scope_databases(local_path, authority_path)
        stage_outbox(local_path, SCOPE)
        authority_admit(authority_path, SCOPE)  # Commit, then lose the response.
        recover_scope(local_path, authority_path)
        recover_scope(local_path, authority_path)
        stage_outbox(local_path, SCOPE_TWO)
        authority_admit(authority_path, SCOPE_TWO)  # Lose the second response too.
        recover_scope(local_path, authority_path)
        recover_scope(local_path, authority_path)
        local_counts = table_counts(
            local_path,
            (
                "scope_admission_outbox",
                "scope_admission_resolutions",
                "admitted_scopes",
                "scope_checkpoints",
                "scope_heads",
                "namespace_checkpoints",
                "namespace_members",
            ),
        )
        require(local_counts == {
            "scope_admission_outbox": 2,
            "scope_admission_resolutions": 2,
            "admitted_scopes": 2,
            "scope_checkpoints": 2,
            "scope_heads": 2,
            "namespace_checkpoints": 2,
            "namespace_members": 3,
        }, f"two-scope local replay multiplied rows: {local_counts}")
        authority_counts = table_counts(
            authority_path, (
                "authority_scopes", "authority_namespace_members",
                "authority_namespace_checkpoints",
                "authority_namespace_snapshot_members",
            )
        )
        require(authority_counts == {
            "authority_scopes": 2,
            "authority_namespace_members": 2,
            "authority_namespace_checkpoints": 2,
            "authority_namespace_snapshot_members": 3,
        }, f"two-scope authority replay multiplied rows: {authority_counts}")

        changed = {**SCOPE, "admittedAt": "2026-07-14T00:00:03.000Z"}
        try:
            authority_admit(authority_path, changed)
        except AdmissionConflict as error:
            require(str(error) == "LIFECYCLE_SCOPE_ADMISSION_CONFLICT",
                    "wrong changed-byte conflict")
        else:
            raise OracleFailure("authority accepted changed bytes for the same scope key")


@case("SA-03")
def every_local_finalization_crash_rolls_back_then_recovers_once() -> None:
    for crash_after in (1, 2, 3, 4, 5, 6):
        with tempfile.TemporaryDirectory(prefix=f"capa001-sa03-{crash_after}-") as directory:
            local_path = Path(directory) / "local.sqlite3"
            authority_path = Path(directory) / "authority.sqlite3"
            initialize_scope_databases(local_path, authority_path)
            request_id, digest = stage_outbox(local_path, SCOPE)
            result = authority_admit(authority_path, SCOPE)
            try:
                finalize_local(local_path, request_id, digest, result, crash_after)
            except SimulatedCrash as error:
                require(str(error) == f"after-local-insert-{crash_after}",
                        "wrong crash boundary")
            else:
                raise OracleFailure(f"crash boundary {crash_after} committed")

            rolled_back = table_counts(
                local_path,
                (
                    "scope_admission_outbox",
                    "scope_admission_resolutions",
                    "admitted_scopes",
                    "scope_checkpoints",
                    "scope_heads",
                    "namespace_checkpoints",
                    "namespace_members",
                ),
            )
            require(rolled_back == {
                "scope_admission_outbox": 1,
                "scope_admission_resolutions": 0,
                "admitted_scopes": 0,
                "scope_checkpoints": 0,
                "scope_heads": 0,
                "namespace_checkpoints": 0,
                "namespace_members": 0,
            }, f"partial local commit at boundary {crash_after}: {rolled_back}")
            recover_scope(local_path, authority_path)
            recovered = table_counts(
                local_path,
                (
                    "scope_admission_outbox",
                    "scope_admission_resolutions",
                    "admitted_scopes",
                    "scope_checkpoints",
                    "scope_heads",
                    "namespace_checkpoints",
                    "namespace_members",
                ),
            )
            require(all(value == 1 for value in recovered.values()),
                    f"recovery not exact at boundary {crash_after}: {recovered}")
    for crash_after in range(1, 8):  # 5 + N writes where the pinned set has N=2.
        with tempfile.TemporaryDirectory(
            prefix=f"capa001-sa03-multiscope-{crash_after}-"
        ) as directory:
            local_path = Path(directory) / "local.sqlite3"
            authority_path = Path(directory) / "authority.sqlite3"
            initialize_scope_databases(local_path, authority_path)
            first_request, first_digest = stage_outbox(local_path, SCOPE)
            finalize_local(
                local_path, first_request, first_digest,
                authority_admit(authority_path, SCOPE),
            )
            request_id, digest = stage_outbox(local_path, SCOPE_TWO)
            result = authority_admit(authority_path, SCOPE_TWO)
            try:
                finalize_local(local_path, request_id, digest, result, crash_after)
            except SimulatedCrash as error:
                require(str(error) == f"after-local-insert-{crash_after}",
                        "wrong multi-scope crash boundary")
            else:
                raise OracleFailure(f"multi-scope boundary {crash_after} committed")
            rolled_back = table_counts(local_path, (
                "scope_admission_outbox", "scope_admission_resolutions",
                "admitted_scopes", "scope_checkpoints", "scope_heads",
                "namespace_checkpoints", "namespace_members",
            ))
            require(rolled_back == {
                "scope_admission_outbox": 2,
                "scope_admission_resolutions": 1,
                "admitted_scopes": 1,
                "scope_checkpoints": 1,
                "scope_heads": 1,
                "namespace_checkpoints": 1,
                "namespace_members": 1,
            }, f"partial multi-scope commit at {crash_after}: {rolled_back}")
            recover_scope(local_path, authority_path)
            recovered = table_counts(local_path, (
                "scope_admission_outbox", "scope_admission_resolutions",
                "admitted_scopes", "scope_checkpoints", "scope_heads",
                "namespace_checkpoints", "namespace_members",
            ))
            require(recovered == {
                "scope_admission_outbox": 2,
                "scope_admission_resolutions": 2,
                "admitted_scopes": 2,
                "scope_checkpoints": 2,
                "scope_heads": 2,
                "namespace_checkpoints": 2,
                "namespace_members": 3,
            }, f"multi-scope recovery not exact at {crash_after}: {recovered}")


def main() -> int:
    passed = 0
    for case_id, function in CASES:
        try:
            function()
        except Exception as error:  # stable first-failure report; traceback is noise here.
            print(f"{case_id}: FAIL: {type(error).__name__}: {error}")
            return 1
        print(f"{case_id}: PASS")
        passed += 1
    print(f"fresh-origin-after: {passed}/{len(CASES)}")
    return 0


def test_fresh_origin_after_oracle(capsys) -> None:
    assert main() == 0
    output = capsys.readouterr().out
    assert f"fresh-origin-after: {len(CASES)}/{len(CASES)}" in output


if __name__ == "__main__":
    raise SystemExit(main())
