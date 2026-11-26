## Behaviour changes
### Accounts, quota and routing

- A launch now checks every requested field against the adapter's declared capabilities before it starts. A missing model, effort or image-input capability degrades with a warning that names the declaration: routing prefers a fallback, an exact pin drops the effort control, and image attachments stay readable paths. `policy.LAUNCH_MODEL_UNSUPPORTED`, `policy.LAUNCH_EFFORT_UNSUPPORTED` and `policy.LAUNCH_IMAGE_INPUT_UNSUPPORTED` (warn by default), or an adapter's `on_gap` entry, set that to `off` or `refuse`. Write, network, resume and skill-location gaps always refuse, naming the adapter, field, reason and source. `policy.LAUNCH_FIELD_UNSUPPORTED`, which no launch read, is removed. (#4035, #4034)
