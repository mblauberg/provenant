## Breaking changes

### Accounts, quota and routing

- Route receipts now decode selection and exclusion evidence as typed fields, with typed candidate ranking and route diagnostics. Upgrade all hosts together for format 2. Malformed known fields fail decoding. (#3078)

## Fixes

### Accounts, quota and routing

- Role-less attempts now contribute outcome priors using the same review, writer or task key as route selection. (#3078)
