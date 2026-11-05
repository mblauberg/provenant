## Behaviour changes
### Lanes and settlement

- Worker census now records typed taints (`TaintCause`), distinguishing lock contention from census uncertainty (unreadable working directories, unplaceable paths, or incomplete listings) so admission policies can retry contention while keeping uncertain roots fail-closed.
- An unreadable or unplaceable process now taints only roots it could occupy given the evidence; affirmative evidence of a different root does not taint unrelated roots, while wholly unplaceable processes fail closed across all candidate roots.
- Live-writer refusals distinguish and name the evidence kind found (`working directory`, `executable image`, or `other held path`) instead of assuming any holding process had its working directory inside the tree (#2760).
