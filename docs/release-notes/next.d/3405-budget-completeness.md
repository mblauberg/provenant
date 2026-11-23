## Fixes

### Lanes and settlement

- Token-budget settlement no longer promotes a truncated provider result to `Succeeded`: one gate decides success from the valid done handoff, and the decoder's completeness is kept, so a non-empty but incomplete answer settles as incomplete. An empty answer still keeps its handoff-fence substitution but settles as a review record. #3405
