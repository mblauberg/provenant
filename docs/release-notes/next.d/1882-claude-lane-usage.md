## Fixes

Claude lane attempts now record the input and output token counts from the
provider's final result event, instead of marking usage missing. Cache read
and cache write counts are not yet recorded for Claude lanes.
