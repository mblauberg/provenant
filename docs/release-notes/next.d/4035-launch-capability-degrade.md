## Fixes

### Lanes and settlement

- Launch capability gaps now degrade or refuse by field instead of refusing every unsupported control. A gap in the model selector, effort, image input or resume drops the control or selects an eligible fallback and records a warning naming the adapter row as source in the applied route's `launch_warnings`, so an exact pin with an unsupported effort still launches and the result envelope shows why the control did not apply. Write, network and skill-location gaps remain refusals before a writer is prepared, because confinement and authority depend on them; they are reported as plain capability refusals, not authority boundaries. (#4035)
