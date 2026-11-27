-- Base 14: v2.8.2 attempt records decode after the provenance and usage renames.
--
-- Base 13 recorded attempt bytes written by v2.8.2, whose usage source was a
-- free-form string with flat token buckets and whose lane, sandbox and resource
-- facts spelled provenance with the retired families. The current types reject
-- those spellings and read the old buckets as unknown extra fields, so status,
-- resume and settlement could not decode the rows. Rewrite only the paths the
-- new types cannot read; every walk is total for any valid JSON and skips a
-- malformed row so it fails only its own decode. Rows already in the new shape
-- are left byte-identical. Terminal rows are immutable to ordinary writers, so
-- the trigger is lifted only inside the migration transaction.
DROP TRIGGER IF EXISTS immutable_terminal_update;

-- Usage observation: v2.8.2 wrote "provider" (or an "absent: ..." note, or a
-- JSON null) as a string. Every such report came from the provider stream,
-- which is the typed default; the current writer always stamps "stream".
UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.usage.source', 'stream') AS BLOB)
WHERE record IS NOT NULL
  AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.usage') = 'object'
  AND (
        json_extract(CAST(record AS TEXT), '$.usage.source') = 'provider'
     OR json_extract(CAST(record AS TEXT), '$.usage.source') LIKE 'absent:%'
     OR json_type(CAST(record AS TEXT), '$.usage.source') = 'null'
  );

-- A retained session total is the same UsageRecord shape and carries the same
-- old buckets and source spelling.
UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.usage.session_total.source', 'stream') AS BLOB)
WHERE record IS NOT NULL
  AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.usage.session_total') = 'object'
  AND (
        json_extract(CAST(record AS TEXT), '$.usage.session_total.source') = 'provider'
     OR json_extract(CAST(record AS TEXT), '$.usage.session_total.source') LIKE 'absent:%'
     OR json_type(CAST(record AS TEXT), '$.usage.session_total.source') = 'null'
  );

-- Move the flat v2.8.2 buckets under `tokens`, keeping every stated number.
-- An absent bucket stays absent (JSON null in json_object decodes as None).
UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.usage.tokens', json_object(
        'input', json_extract(CAST(record AS TEXT), '$.usage.uncached_input'),
        'output', json_extract(CAST(record AS TEXT), '$.usage.output_excluding_reasoning'),
        'cache_read', json_extract(CAST(record AS TEXT), '$.usage.cache_read'),
        'cache_write', json_extract(CAST(record AS TEXT), '$.usage.cache_write'),
        'cache_write_1h', json_extract(CAST(record AS TEXT), '$.usage.cache_write_1h'),
        'reasoning', json_extract(CAST(record AS TEXT), '$.usage.reasoning')
    )) AS BLOB)
WHERE record IS NOT NULL
  AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.usage') = 'object'
  AND json_extract(CAST(record AS TEXT), '$.usage.tokens') IS NULL
  AND (
        json_type(CAST(record AS TEXT), '$.usage.uncached_input') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.output_excluding_reasoning') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.cache_read') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.cache_write') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.cache_write_1h') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.reasoning') IS NOT NULL
  );

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.usage.session_total.tokens', json_object(
        'input', json_extract(CAST(record AS TEXT), '$.usage.session_total.uncached_input'),
        'output', json_extract(CAST(record AS TEXT), '$.usage.session_total.output_excluding_reasoning'),
        'cache_read', json_extract(CAST(record AS TEXT), '$.usage.session_total.cache_read'),
        'cache_write', json_extract(CAST(record AS TEXT), '$.usage.session_total.cache_write'),
        'cache_write_1h', json_extract(CAST(record AS TEXT), '$.usage.session_total.cache_write_1h'),
        'reasoning', json_extract(CAST(record AS TEXT), '$.usage.session_total.reasoning')
    )) AS BLOB)
WHERE record IS NOT NULL
  AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.usage.session_total') = 'object'
  AND json_extract(CAST(record AS TEXT), '$.usage.session_total.tokens') IS NULL
  AND (
        json_type(CAST(record AS TEXT), '$.usage.session_total.uncached_input') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.session_total.output_excluding_reasoning') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.session_total.cache_read') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.session_total.cache_write') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.session_total.cache_write_1h') IS NOT NULL
     OR json_type(CAST(record AS TEXT), '$.usage.session_total.reasoning') IS NOT NULL
  );

-- Retire the old flat bucket keys only after their values live under tokens.
UPDATE attempts
SET record = CAST(json_remove(
        CAST(record AS TEXT),
        '$.usage.uncached_input', '$.usage.output_excluding_reasoning',
        '$.usage.cache_read', '$.usage.cache_write', '$.usage.cache_write_1h',
        '$.usage.reasoning',
        '$.usage.session_total.uncached_input',
        '$.usage.session_total.output_excluding_reasoning',
        '$.usage.session_total.cache_read', '$.usage.session_total.cache_write',
        '$.usage.session_total.cache_write_1h', '$.usage.session_total.reasoning'
    ) AS BLOB)
WHERE record IS NOT NULL
  AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.usage.tokens') IS NOT NULL
  AND json_type(CAST(record AS TEXT), '$.usage') = 'object';

-- Lane access: the resolved route's policy is Layer::Route, not the retired
-- `route_policy` spelling. Apply it to every route snapshot the record keeps.
UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.resolved_route.lane_access.source', 'route') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.resolved_route.lane_access.source') = 'route_policy';

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.applied_route.lane_access.source', 'route') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.applied_route.lane_access.source') = 'route_policy';

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.observed_route.lane_access.source', 'route') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.observed_route.lane_access.source') = 'route_policy';

-- Sandbox mode: a platform or restriction cap became a `constraint` beside the
-- Layer::Host source, and a knob reports its actual host layer. Record the cap
-- before replacing the source, so the guard still sees the retired spelling.
UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.resolved_route.sandbox.constraint',
        json_extract(CAST(record AS TEXT), '$.resolved_route.sandbox.source')) AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.resolved_route.sandbox.source') IN ('restriction','platform');

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.resolved_route.sandbox.source', 'host') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.resolved_route.sandbox.source') IN ('restriction','platform','knob');

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.applied_route.sandbox.constraint',
        json_extract(CAST(record AS TEXT), '$.applied_route.sandbox.source')) AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.applied_route.sandbox.source') IN ('restriction','platform');

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.applied_route.sandbox.source', 'host') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.applied_route.sandbox.source') IN ('restriction','platform','knob');

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.observed_route.sandbox.constraint',
        json_extract(CAST(record AS TEXT), '$.observed_route.sandbox.source')) AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.observed_route.sandbox.source') IN ('restriction','platform');

UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.observed_route.sandbox.source', 'host') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.observed_route.sandbox.source') IN ('restriction','platform','knob');

-- A retained resource fact on a skipped route named its setting's origin with
-- the retired SettingSource spellings; a shipped default is the Default layer
-- and a host setting is the Host layer. PacingReserve and LanePorts used their
-- own setting-source enums, now replaced by Layer, so their old spellings are
-- rewritten too. Every ResourceFact whose `origin` is a Layer is covered.
UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.skipped_routes', json((
        SELECT json_group_array(
            CASE
                WHEN json_extract(step.value, '$.guard_resource.kind') IN (
                        'disk','argv_prompt_limit','continuation_copy',
                        'write_grant_limit','grant_limit','profile_limit',
                        'memory_pressure','lane_cap','pacing_reserve','lane_ports')
                    AND json_extract(step.value, '$.guard_resource.origin') = 'configuration'
                    THEN json(json_set(step.value, '$.guard_resource.origin', 'host'))
                WHEN json_extract(step.value, '$.guard_resource.kind') IN (
                        'disk','argv_prompt_limit','continuation_copy',
                        'write_grant_limit','grant_limit','profile_limit',
                        'memory_pressure','lane_cap','pacing_reserve','lane_ports')
                    AND json_extract(step.value, '$.guard_resource.origin') = 'shipped_default'
                    THEN json(json_set(step.value, '$.guard_resource.origin', 'default'))
                ELSE json(step.value)
            END
        )
        FROM (
            SELECT key, value
            FROM json_each(CAST(record AS TEXT), '$.skipped_routes')
            ORDER BY key
        ) AS step
    ))) AS BLOB)
WHERE record IS NOT NULL
  AND json_valid(CAST(record AS TEXT))
  AND json_type(CAST(record AS TEXT), '$.skipped_routes') = 'array'
  AND EXISTS (
        SELECT 1 FROM json_each(CAST(record AS TEXT), '$.skipped_routes') AS step
        WHERE json_extract(step.value, '$.guard_resource.kind') IN (
                  'disk','argv_prompt_limit','continuation_copy',
                  'write_grant_limit','grant_limit','profile_limit',
                  'memory_pressure','lane_cap','pacing_reserve','lane_ports')
          AND json_extract(step.value, '$.guard_resource.origin') IN ('configuration','shipped_default')
  );

-- A repository port declared by the built-in table reported PortSource::Builtin.
UPDATE attempts
SET record = CAST(json_set(CAST(record AS TEXT), '$.repository_admission_binding.source', 'default') AS BLOB)
WHERE record IS NOT NULL AND json_valid(CAST(record AS TEXT))
  AND json_extract(CAST(record AS TEXT), '$.repository_admission_binding.source') = 'builtin';

CREATE TRIGGER immutable_terminal_update
 BEFORE UPDATE OF rowid,oid,_rowid_,sequence,id,project,graph,node,attempt,digest,actor,operation,number,continues,refused,revision,terminal,record,sealed,repository,head ON attempts
 WHEN OLD.terminal=1
 BEGIN SELECT RAISE(ABORT,'terminal attempt is immutable'); END;
