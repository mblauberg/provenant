## Fixes
### Lanes and settlement

- Agy lanes that report a refused action with a successful empty result now settle as `permission_denied`, naming the action. Reported denials that may have been recovered are recorded as notes; Claude does not identify whether a denial ended the work. Silent denials, such as an empty glob, cannot be detected (#3079).

## Additions
### Lanes and settlement
