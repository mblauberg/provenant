## Fixes

### Lanes and settlement

- Structured JSONL provider stream redaction preserves frame structure by masking inside string values, and unparseable frames report an evidence defect naming their line rather than settling as generic task failures. (#3395)
