## Fixes

Graph nodes can declare `required_verdict`, an exact, case-sensitive result line (surrounding whitespace is ignored). For example, set `"required_verdict": "VERDICT: PASS"` and ask for that line in the prompt. A successful lane without the line fails the node, blocks dependants and prevents graph delivery. Non-empty text alone does not satisfy the contract. Nodes without the field keep their existing behaviour, and existing graph checkpoints remain compatible.
